use super::webauthn::{self, b64_decode, b64_encode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

pub const PASSKEYS_FILE: &str = "passkeys.json";
pub const SESSIONS_FILE: &str = "web-sessions.json";
pub const SESSION_COOKIE: &str = "__Host-godterm_session";
pub const SESSION_COOKIE_PLAIN: &str = "godterm_session";

pub const CHALLENGE_TTL: i64 = 300;
pub const PAIR_TTL: i64 = 600;
pub const SESSION_TTL: i64 = 30 * 86400;
pub const SESSION_MAX: i64 = 90 * 86400;
pub const MAX_PENDING: usize = 128;
pub const MAX_SESSIONS: usize = 50;
pub const MAX_PASSKEYS: usize = 20;
const TOUCH_EVERY: i64 = 60;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Passkey {
    pub id: String,
    pub key: String,
    pub label: String,
    pub rp: String,
    pub created: i64,
    pub last_used: i64,
    pub counter: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
struct Saved {
    enabled: bool,
    remote_url: String,
    recovery: String,
    handle: String,
    passkeys: Vec<Passkey>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SessionRec {
    pub hash: String,
    pub label: String,
    pub born: i64,
    pub last_seen: i64,
    pub expires: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Register,
    Login,
}

struct Pending {
    cid: String,
    challenge: Vec<u8>,
    kind: Kind,
    expires: i64,
}

type Sig = Option<(SystemTime, u64)>;

struct Inner {
    saved: Saved,
    sessions: Vec<SessionRec>,
    pending: Vec<Pending>,
    pairs: Vec<(String, i64)>,
    sig_saved: Sig,
    sig_sessions: Sig,
    flushed: i64,
    dirty: bool,
}

pub struct Login {
    dir: PathBuf,
    inner: Mutex<Inner>,
    limiter: Mutex<std::collections::HashMap<String, super::auth::Limiter>>,
}

fn sig(p: &Path) -> Sig {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn write_private(p: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use crate::platform::OpenOptionsExt;
    use std::io::Write;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut tmp = p.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
    }
    std::fs::rename(&tmp, p)
}

fn random(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    crate::platform::fill_random(&mut b);
    b
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha_hex(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

fn sid_of(hash: &str) -> String {
    hash.chars().take(12).collect()
}

pub fn parse_origin(url: &str) -> Result<(String, String), String> {
    let url = url.trim().trim_end_matches('/');
    let (scheme, rest) = url.split_once("://").ok_or("the address needs https://")?;
    if rest.is_empty() || rest.contains(['/', '@', '?', '#', ' ']) {
        return Err("the address must be just a host name".into());
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (rest, None),
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        || host.starts_with(['.', '-'])
        || host.ends_with(['.', '-'])
    {
        return Err("the host name is not valid".into());
    }
    if host.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err("a passkey needs a host name, not an IP address".into());
    }
    let port = match port {
        Some(p) => Some(
            p.parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or("the port is not valid")?,
        ),
        None => None,
    };
    match scheme {
        "https" => {
            if !host.contains('.') {
                return Err("the host name is not valid".into());
            }
            let origin = match port {
                Some(p) if p != 443 => format!("https://{host}:{p}"),
                _ => format!("https://{host}"),
            };
            Ok((origin, host))
        }
        "http" if host == "localhost" => {
            let origin = match port {
                Some(p) if p != 80 => format!("http://{host}:{p}"),
                _ => format!("http://{host}"),
            };
            Ok((origin, host))
        }
        _ => Err("the address must be https".into()),
    }
}

pub fn device_label(ua: &str) -> String {
    let os = ["iPhone", "iPad", "Android", "Macintosh", "Windows", "Linux"]
        .iter()
        .find(|o| ua.contains(**o))
        .map(|o| if *o == "Macintosh" { "Mac" } else { *o })
        .unwrap_or("Device");
    let app = if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("Firefox/") || ua.contains("FxiOS") {
        "Firefox"
    } else if ua.contains("Chrome/") || ua.contains("CriOS") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else {
        "browser"
    };
    format!("{os}, {app}")
}

pub fn clean_label(s: &str) -> String {
    let t: String = s
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect::<String>()
        .trim()
        .to_string();
    t
}

impl Login {
    pub fn open(dir: PathBuf) -> Login {
        let l = Login {
            dir,
            inner: Mutex::new(Inner {
                saved: Saved::default(),
                sessions: vec![],
                pending: vec![],
                pairs: vec![],
                sig_saved: None,
                sig_sessions: None,
                flushed: 0,
                dirty: false,
            }),
            limiter: Mutex::new(std::collections::HashMap::new()),
        };
        l.lock().sig_saved = Some((SystemTime::UNIX_EPOCH, u64::MAX));
        l.sync();
        l
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn saved_path(&self) -> PathBuf {
        self.dir.join(PASSKEYS_FILE)
    }

    fn sessions_path(&self) -> PathBuf {
        self.dir.join(SESSIONS_FILE)
    }

    pub fn sync(&self) {
        let mut g = self.lock();
        let (sp, kp) = (self.saved_path(), self.sessions_path());
        let (s1, s2) = (sig(&sp), sig(&kp));
        if s1 != g.sig_saved {
            g.saved = std::fs::read(&sp)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            g.sig_saved = s1;
        }
        if s2 != g.sig_sessions {
            g.sessions = std::fs::read(&kp)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            g.sig_sessions = s2;
        }
    }

    fn save(&self, g: &mut Inner) {
        if let Ok(b) = serde_json::to_vec_pretty(&g.saved) {
            let p = self.saved_path();
            let _ = write_private(&p, &b);
            g.sig_saved = sig(&p);
        }
    }

    fn save_sessions(&self, g: &mut Inner, now: i64) {
        if let Ok(b) = serde_json::to_vec(&g.sessions) {
            let p = self.sessions_path();
            let _ = write_private(&p, &b);
            g.sig_sessions = sig(&p);
        }
        g.flushed = now;
        g.dirty = false;
    }

    pub fn enabled(&self) -> bool {
        self.sync();
        self.lock().saved.enabled
    }

    pub fn remote(&self) -> Option<(String, String)> {
        self.sync();
        let g = self.lock();
        parse_origin(&g.saved.remote_url).ok()
    }

    pub fn blocked(&self, key: &str) -> bool {
        let mut m = self.limiter.lock().unwrap_or_else(|e| e.into_inner());
        m.get_mut(key).is_some_and(|l| l.blocked(Instant::now()))
    }

    pub fn fail(&self, key: &str) {
        let mut m = self.limiter.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if m.len() >= 1024 && !m.contains_key(key) {
            m.retain(|_, l| l.blocked(now));
            if m.len() >= 1024 {
                m.clear();
            }
        }
        m.entry(key.to_string()).or_default().fail(now);
    }

    pub fn set_remote_url(&self, url: &str) -> Result<String, String> {
        self.sync();
        let (origin, _) = parse_origin(url)?;
        let mut g = self.lock();
        if g.saved.enabled {
            let cur = parse_origin(&g.saved.remote_url).map(|c| c.0).ok();
            if cur.as_deref() != Some(origin.as_str()) {
                return Err("turn login off before changing the address".into());
            }
        }
        g.saved.remote_url = origin.clone();
        self.save(&mut g);
        Ok(origin)
    }

    pub fn clear_remote_url(&self) -> Result<(), String> {
        self.sync();
        let mut g = self.lock();
        if g.saved.enabled {
            return Err("turn login off before removing the address".into());
        }
        g.saved.remote_url.clear();
        self.save(&mut g);
        Ok(())
    }

    fn push_pending(g: &mut Inner, kind: Kind, challenge: Vec<u8>, now: i64) -> String {
        g.pending.retain(|p| p.expires > now);
        while g.pending.len() >= MAX_PENDING {
            g.pending.remove(0);
        }
        let cid = hex(&random(16));
        g.pending.push(Pending {
            cid: cid.clone(),
            challenge,
            kind,
            expires: now + CHALLENGE_TTL,
        });
        cid
    }

    fn take_pending(g: &mut Inner, cid: &str, kind: Kind, now: i64) -> Option<Vec<u8>> {
        let i = g
            .pending
            .iter()
            .position(|p| super::auth::same(p.cid.as_bytes(), cid.as_bytes()))?;
        let p = g.pending.remove(i);
        (p.kind == kind && p.expires > now).then_some(p.challenge)
    }

    pub fn register_begin(&self, now: i64) -> Result<Value, String> {
        self.sync();
        let rp = self.remote().ok_or("create the remote address first")?.1;
        let mut g = self.lock();
        if g.saved.passkeys.len() >= MAX_PASSKEYS {
            return Err("too many passkeys".into());
        }
        if g.saved.handle.is_empty() {
            g.saved.handle = b64_encode(&random(32));
            self.save(&mut g);
        }
        let challenge = random(32);
        let cid = Self::push_pending(&mut g, Kind::Register, challenge.clone(), now);
        let exclude: Vec<Value> = g
            .saved
            .passkeys
            .iter()
            .filter(|p| p.rp == rp)
            .map(|p| json!({"type": "public-key", "id": p.id}))
            .collect();
        Ok(json!({
            "cid": cid,
            "publicKey": {
                "rp": {"id": rp, "name": "GodTerm"},
                "user": {"id": g.saved.handle, "name": "godterm", "displayName": "GodTerm"},
                "challenge": b64_encode(&challenge),
                "pubKeyCredParams": [{"type": "public-key", "alg": -7}],
                "timeout": 300000,
                "attestation": "none",
                "authenticatorSelection": {
                    "residentKey": "required",
                    "requireResidentKey": true,
                    "userVerification": "required"
                },
                "excludeCredentials": exclude
            }
        }))
    }

    pub fn register_finish(
        &self,
        cid: &str,
        label: &str,
        client_data: &str,
        attestation: &str,
        now: i64,
    ) -> Result<Value, String> {
        self.sync();
        let bad = || "the passkey was not accepted".to_string();
        let (origin, rp) = self.remote().ok_or_else(bad)?;
        let cd = b64_decode(client_data).ok_or_else(bad)?;
        let att = b64_decode(attestation).ok_or_else(bad)?;
        let mut g = self.lock();
        let challenge = Self::take_pending(&mut g, cid, Kind::Register, now).ok_or_else(bad)?;
        let r =
            webauthn::verify_registration(&cd, &att, &challenge, &origin, &rp).map_err(|e| {
                crate::log::info(&format!("web: a passkey registration was refused: {e}"));
                bad()
            })?;
        let id = b64_encode(&r.id);
        if g.saved.passkeys.iter().any(|p| p.id == id) {
            return Err(bad());
        }
        if g.saved.passkeys.len() >= MAX_PASSKEYS {
            return Err(bad());
        }
        let label = clean_label(label);
        let pk = Passkey {
            id,
            key: b64_encode(&r.key),
            label: if label.is_empty() {
                format!("Passkey {}", g.saved.passkeys.len() + 1)
            } else {
                label
            },
            rp,
            created: now,
            last_used: 0,
            counter: r.counter,
        };
        let view = passkey_view(&pk, true);
        g.saved.passkeys.push(pk);
        self.save(&mut g);
        Ok(view)
    }

    pub fn login_begin(&self, now: i64) -> Result<Value, String> {
        self.sync();
        let (_, rp) = self.remote().ok_or("login is not set up")?;
        let mut g = self.lock();
        if !g.saved.enabled {
            return Err("login is off".into());
        }
        let challenge = random(32);
        let cid = Self::push_pending(&mut g, Kind::Login, challenge.clone(), now);
        Ok(json!({
            "cid": cid,
            "publicKey": {
                "challenge": b64_encode(&challenge),
                "rpId": rp,
                "timeout": 300000,
                "userVerification": "required",
                "allowCredentials": []
            }
        }))
    }

    pub fn login_finish(
        &self,
        cid: &str,
        cred_id: &str,
        client_data: &str,
        auth_data: &str,
        signature: &str,
        label: &str,
        now: i64,
    ) -> Result<String, ()> {
        self.sync();
        let (origin, rp) = self.remote().ok_or(())?;
        let cd = b64_decode(client_data).ok_or(())?;
        let ad = b64_decode(auth_data).ok_or(())?;
        let sg = b64_decode(signature).ok_or(())?;
        let mut g = self.lock();
        if !g.saved.enabled {
            return Err(());
        }
        let challenge = Self::take_pending(&mut g, cid, Kind::Login, now).ok_or(())?;
        let pk = g
            .saved
            .passkeys
            .iter()
            .find(|p| p.rp == rp && super::auth::same(p.id.as_bytes(), cred_id.as_bytes()))
            .cloned()
            .ok_or(())?;
        let key = b64_decode(&pk.key).ok_or(())?;
        let counter =
            webauthn::verify_assertion(&cd, &ad, &sg, &challenge, &origin, &rp, &key, pk.counter)
                .map_err(|e| {
                crate::log::info(&format!("web: a passkey sign in was refused: {e}"));
            })?;
        if let Some(p) = g.saved.passkeys.iter_mut().find(|p| p.id == pk.id) {
            p.counter = counter;
            p.last_used = now;
        }
        self.save(&mut g);
        Ok(Self::issue_in(self, &mut g, label, now))
    }

    fn issue_in(&self, g: &mut Inner, label: &str, now: i64) -> String {
        g.sessions.retain(|s| s.expires > now);
        g.sessions.sort_by_key(|s| s.last_seen);
        while g.sessions.len() >= MAX_SESSIONS {
            g.sessions.remove(0);
        }
        let value = hex(&random(32));
        g.sessions.push(SessionRec {
            hash: sha_hex(&value),
            label: clean_label(label),
            born: now,
            last_seen: now,
            expires: now + SESSION_TTL,
        });
        self.save_sessions(g, now);
        value
    }

    pub fn issue_session(&self, label: &str, now: i64) -> String {
        self.sync();
        let mut g = self.lock();
        Self::issue_in(self, &mut g, label, now)
    }

    pub fn check_session(&self, value: &str, now: i64) -> Option<String> {
        if value.len() != 64 {
            return None;
        }
        self.sync();
        let hash = sha_hex(value);
        let mut g = self.lock();
        let i = g
            .sessions
            .iter()
            .position(|s| super::auth::same(s.hash.as_bytes(), hash.as_bytes()))?;
        if g.sessions[i].expires <= now {
            return None;
        }
        let s = &mut g.sessions[i];
        let sid = sid_of(&s.hash);
        if now - s.last_seen >= TOUCH_EVERY {
            s.last_seen = now;
            s.expires = (now + SESSION_TTL).min(s.born + SESSION_MAX).max(s.expires);
            g.dirty = true;
        }
        if g.dirty && now - g.flushed >= TOUCH_EVERY {
            self.save_sessions(&mut g, now);
        }
        Some(sid)
    }

    pub fn session_live(&self, sid: &str, now: i64) -> bool {
        self.sync();
        self.lock()
            .sessions
            .iter()
            .any(|s| s.expires > now && sid_of(&s.hash) == sid)
    }

    pub fn sessions(&self, now: i64) -> Vec<Value> {
        self.sync();
        let g = self.lock();
        let mut v: Vec<&SessionRec> = g.sessions.iter().filter(|s| s.expires > now).collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.last_seen));
        v.iter()
            .map(|s| {
                json!({"sid": sid_of(&s.hash), "label": s.label, "created": s.born,
                    "last_seen": s.last_seen, "expires": s.expires})
            })
            .collect()
    }

    pub fn revoke_session(&self, sid: &str, now: i64) -> bool {
        self.sync();
        let mut g = self.lock();
        let n = g.sessions.len();
        g.sessions.retain(|s| sid_of(&s.hash) != sid);
        let gone = g.sessions.len() != n;
        if gone {
            self.save_sessions(&mut g, now);
        }
        gone
    }

    pub fn revoke_all(&self, keep: Option<&str>, now: i64) -> usize {
        self.sync();
        let mut g = self.lock();
        let n = g.sessions.len();
        g.sessions
            .retain(|s| Some(sid_of(&s.hash).as_str()) == keep);
        let gone = n - g.sessions.len();
        self.save_sessions(&mut g, now);
        gone
    }

    pub fn passkeys(&self) -> Vec<Value> {
        self.sync();
        let rp = self.remote().map(|r| r.1).unwrap_or_default();
        self.lock()
            .saved
            .passkeys
            .iter()
            .map(|p| passkey_view(p, p.rp == rp))
            .collect()
    }

    pub fn rename_passkey(&self, pid: &str, label: &str) -> bool {
        self.sync();
        let label = clean_label(label);
        if label.is_empty() {
            return false;
        }
        let mut g = self.lock();
        let Some(p) = g.saved.passkeys.iter_mut().find(|p| pid_of(&p.id) == pid) else {
            return false;
        };
        p.label = label;
        self.save(&mut g);
        true
    }

    pub fn remove_passkey(&self, pid: &str) -> Result<(), String> {
        self.sync();
        let mut g = self.lock();
        let Some(i) = g.saved.passkeys.iter().position(|p| pid_of(&p.id) == pid) else {
            return Err("no such passkey".into());
        };
        if g.saved.enabled && g.saved.passkeys.len() == 1 {
            return Err("login is on: add another passkey first, or turn login off".into());
        }
        g.saved.passkeys.remove(i);
        self.save(&mut g);
        Ok(())
    }

    fn make_recovery(g: &mut Inner) -> String {
        const A: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let raw = random(20);
        let code: String = raw.iter().map(|b| A[(*b & 31) as usize] as char).collect();
        g.saved.recovery = sha_hex(&code);
        code.as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("-")
    }

    pub fn new_recovery(&self) -> String {
        self.sync();
        let mut g = self.lock();
        let c = Self::make_recovery(&mut g);
        self.save(&mut g);
        c
    }

    pub fn use_recovery(&self, code: &str, label: &str, now: i64) -> Option<String> {
        self.sync();
        let norm: String = code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let mut g = self.lock();
        if !g.saved.enabled || g.saved.recovery.is_empty() {
            return None;
        }
        if !super::auth::same(sha_hex(&norm).as_bytes(), g.saved.recovery.as_bytes()) {
            return None;
        }
        g.saved.recovery.clear();
        self.save(&mut g);
        Some(Self::issue_in(self, &mut g, label, now))
    }

    pub fn enable(&self) -> Result<String, String> {
        self.sync();
        let rp = self.remote().ok_or("create the remote address first")?.1;
        let mut g = self.lock();
        if g.saved.enabled {
            return Err("login is already on".into());
        }
        if !g.saved.passkeys.iter().any(|p| p.rp == rp) {
            return Err("add a passkey first".into());
        }
        g.saved.enabled = true;
        let code = Self::make_recovery(&mut g);
        self.save(&mut g);
        g.pending.clear();
        Ok(code)
    }

    pub fn disable(&self, now: i64) {
        self.sync();
        let mut g = self.lock();
        g.saved.enabled = false;
        g.saved.recovery.clear();
        self.save(&mut g);
        g.sessions.clear();
        self.save_sessions(&mut g, now);
    }

    pub fn new_pair(&self, now: i64) -> String {
        let mut g = self.lock();
        g.pairs.retain(|p| p.1 > now);
        while g.pairs.len() >= 8 {
            g.pairs.remove(0);
        }
        let code = hex(&random(24));
        g.pairs.push((sha_hex(&code), now + PAIR_TTL));
        code
    }

    pub fn use_pair(&self, code: &str, now: i64) -> bool {
        let h = sha_hex(code);
        let mut g = self.lock();
        g.pairs.retain(|p| p.1 > now);
        let Some(i) = g
            .pairs
            .iter()
            .position(|p| super::auth::same(p.0.as_bytes(), h.as_bytes()))
        else {
            return false;
        };
        g.pairs.remove(i);
        true
    }

    pub fn status(&self, now: i64) -> Value {
        self.sync();
        let remote = self.remote();
        let g = self.lock();
        let live = g.sessions.iter().filter(|s| s.expires > now).count();
        json!({
            "enabled": g.saved.enabled,
            "remote_url": remote.as_ref().map(|r| r.0.clone()).unwrap_or_default(),
            "passkeys": g.saved.passkeys.len(),
            "sessions": live,
            "recovery": !g.saved.recovery.is_empty(),
        })
    }

    pub fn reset(dir: &Path, now: i64) -> bool {
        let l = Login::open(dir.to_path_buf());
        let was = l.enabled();
        l.disable(now);
        was
    }
}

pub fn pid_of(id: &str) -> String {
    sha_hex(id).chars().take(12).collect()
}

fn passkey_view(p: &Passkey, current: bool) -> Value {
    json!({"pid": pid_of(&p.id), "label": p.label, "created": p.created,
        "last_used": p.last_used, "rp": p.rp, "current": current})
}

pub fn summary(dir: &Path) -> Value {
    Login::open(dir.to_path_buf()).status(now())
}

pub fn session_from_cookie(header: &str) -> Option<&str> {
    super::auth::cookie_in(header, SESSION_COOKIE)
        .or_else(|| super::auth::cookie_in(header, SESSION_COOKIE_PLAIN))
}

pub fn set_session_cookie(value: &str, secure: bool) -> String {
    if secure {
        format!("{SESSION_COOKIE}={value}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={SESSION_TTL}")
    } else {
        format!("{SESSION_COOKIE_PLAIN}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={SESSION_TTL}")
    }
}

pub fn clear_session_cookie(secure: bool) -> String {
    if secure {
        format!("{SESSION_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0")
    } else {
        format!("{SESSION_COOKIE_PLAIN}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
    }
}

pub fn command(args: &[String]) -> anyhow::Result<()> {
    let dir = crate::config::app_home();
    match args.first().map(String::as_str) {
        Some("reset") => {
            let was = Login::reset(&dir, now());
            let cfg = crate::config::Config::load_or_init().unwrap_or_default();
            let port = super::port(&cfg);
            match super::remote::make_private(&super::remote::system_run, port) {
                Ok(r) if !r.url.is_empty() => {
                    println!(
                        "The remote address {} is now private to your tailnet.",
                        r.url
                    )
                }
                Ok(_) => {}
                Err(e) => println!("Could not close Funnel ({e}); run: tailscale funnel reset"),
            }
            println!(
                "{}",
                if was {
                    "Login is off. All sessions are signed out. Passkeys are kept; turn login on again in the web app (Settings, Remote access and login)."
                } else {
                    "Login was already off. All sessions are signed out."
                }
            );
            println!("Restart is not needed: a running GodTerm notices within seconds.");
            Ok(())
        }
        Some("status") | None => {
            let v = summary(&dir);
            println!(
                "login:     {}",
                if v["enabled"] == true { "on" } else { "off" }
            );
            let url = v["remote_url"].as_str().unwrap_or("");
            println!("address:   {}", if url.is_empty() { "none" } else { url });
            println!("passkeys:  {}", v["passkeys"]);
            println!("sessions:  {}", v["sessions"]);
            println!();
            println!("Turn it on or off in the web app: Settings, Remote access and login.");
            println!("If you are locked out: godterm login reset");
            Ok(())
        }
        Some(x) => anyhow::bail!("godterm login: unknown '{x}' (status | reset)"),
    }
}
