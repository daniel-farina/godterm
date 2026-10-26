//! The live view of a tab's browser in the web app (`bview` frames,
//! docs/WEB_APP.md): the page the tab's agent drives, as pictures that
//! refresh while someone watches, and clicks, scrolls, history and typing
//! sent back through `bsk`.
//!
//! It never starts a `bsk` session on its own: it attaches to the session
//! the tab's agent already has (one on the tab's own browser, named in the
//! agent's transcript or the only one there), or to one the user started
//! with the view's Start button. Sessions on other browsers are never
//! looked at. One capture loop runs per session, shared by everyone
//! watching it, only while someone watches (each viewer says it still
//! does every 20 s; a closed socket or 90 s of silence drops it), with no
//! two captures at once and at most about 1.5 frames a second.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::browser::{self, Browser, ChromeProfile};
use crate::web::tap::Outbox;

/// Binary frame kind of a picture (docs/WEB_APP.md).
pub const KIND_PICTURE: u8 = 5;

/// A viewer that has not said it still watches for this long is dropped.
const KEEP_FOR: Duration = Duration::from_secs(90);
/// The fastest and slowest the pictures refresh: twice the time a capture
/// takes, within these.
const MIN_PERIOD: Duration = Duration::from_millis(650);
const MAX_PERIOD: Duration = Duration::from_secs(3);
/// URL and title are asked for this often.
const META_EVERY: Duration = Duration::from_secs(2);
/// After an action the next picture comes this soon.
const AFTER_ACTION: Duration = Duration::from_millis(250);
/// Pictures wider than this are scaled down.
const MAX_WIDTH: u32 = 1440;
/// The longest one `bsk` call of the view may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

// ---------- parsing ----------

/// One `bsk session list --json` entry.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BskSession {
    pub id: String,
    pub browser: String,
    pub name: String,
    pub created_at_ms: u64,
}

fn str_of(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| match v.get(*k) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn list_of<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .or_else(|| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// `bsk session list --json`: an array (or `{sessions: [...]}`). Entries
/// without an id are dropped; garbage gives nothing.
pub fn parse_sessions(text: &str) -> Vec<BskSession> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    list_of(&v, "sessions")
        .iter()
        .map(|x| BskSession {
            id: str_of(x, &["session_id", "id"]),
            browser: str_of(x, &["browser_instance_id", "instance_id", "browser"]),
            name: str_of(x, &["name"]),
            created_at_ms: x.get("created_at_ms").and_then(Value::as_u64).unwrap_or(0),
        })
        .filter(|s| !s.id.is_empty())
        .collect()
}

/// A tab of the session's Agent Window.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageTab {
    pub id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
}

/// `bsk tab list --json`: an array (or `{tabs: [...]}`).
pub fn parse_tabs(text: &str) -> Vec<PageTab> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    list_of(&v, "tabs")
        .iter()
        .filter(|x| x.is_object())
        .map(|x| PageTab {
            id: str_of(x, &["tab_id", "id"]),
            url: str_of(x, &["url"]),
            title: str_of(x, &["title"]),
            active: ["active", "is_active", "selected", "focused"]
                .iter()
                .any(|k| x.get(*k).and_then(Value::as_bool) == Some(true)),
        })
        .collect()
}

/// The tab the pictures show: the active one, else the first.
pub fn current_tab(tabs: &[PageTab]) -> Option<&PageTab> {
    tabs.iter().find(|t| t.active).or_else(|| tabs.first())
}

/// Width and height of a PNG (its IHDR) or a JPEG (its first SOF).
pub fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() >= 24 && b.starts_with(b"\x89PNG\r\n\x1a\n") && &b[12..16] == b"IHDR" {
        let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
        let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
        return Some((w, h));
    }
    if b.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xff {
                return None;
            }
            let m = b[i + 1];
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            if (0xc0..=0xcf).contains(&m) && ![0xc4, 0xc8, 0xcc].contains(&m) {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

/// The `--session <id>` of a bsk command (a literal id, not `$VAR`).
fn session_arg(cmd: &str) -> Option<String> {
    let w = crate::tab_browser::shell_words(cmd);
    let mut found = None;
    for (i, a) in w.iter().enumerate() {
        let v = if a == "--session" {
            w.get(i + 1).cloned()
        } else {
            a.strip_prefix("--session=").map(str::to_string)
        };
        if let Some(v) = v.filter(|v| valid_id(v)) {
            found = Some(v);
        }
    }
    found
}

/// `"session_id": "<id>"` in a command's output.
fn session_in_output(s: &str) -> Option<String> {
    let mut found = None;
    let mut rest = s;
    while let Some(i) = rest.find("\"session_id\"") {
        rest = &rest[i + 12..];
        let r = rest.trim_start().strip_prefix(':').map(str::trim_start);
        if let Some(v) = r.and_then(|r| r.strip_prefix('"')) {
            if let Some(end) = v.find('"') {
                if valid_id(&v[..end]) {
                    found = Some(v[..end].to_string());
                }
            }
        }
    }
    found
}

/// The session the tab's agent used last, from the tail of its transcript
/// (JSONL): a `--session <id>` in a bsk command it ran, or the session id
/// a `bsk session start --json` printed for it.
pub fn last_session(tail: &str) -> Option<String> {
    for l in tail.lines().rev() {
        if !l.contains("session") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        let mut found = None;
        walk_strings(&v, &mut |s| {
            let hit = if crate::tab_browser::bsk_summary(s).is_some() {
                session_arg(s)
            } else if s.contains("\"session_id\"") {
                session_in_output(s)
            } else {
                None
            };
            if hit.is_some() {
                found = hit;
            }
        });
        if found.is_some() {
            return found;
        }
    }
    None
}

fn walk_strings(v: &Value, f: &mut impl FnMut(&str)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(a) => a.iter().for_each(|x| walk_strings(x, f)),
        Value::Object(o) => o.values().for_each(|x| walk_strings(x, f)),
        _ => {}
    }
}

/// A bsk id: short, letters, digits, `-`, `_`, `.` and `:` only (so it can
/// go into an argument list and a log line as it is).
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
}

// ---------- which session ----------

/// What the view of a tab attaches to.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    /// This session, and why it is the tab's.
    Session(String, &'static str),
    /// None: the agent has not opened the browser.
    NoSession,
    /// Several sessions on the tab's browser and nothing says which is
    /// this tab's.
    Unsure(usize),
}

/// The tab's session among `sessions`: only those on `instances` (the
/// tab's own browser) count. One the user started from this view comes
/// first, then the one the agent's transcript names, then the only one
/// there (unless another tab uses the same browser).
pub fn choose(
    sessions: &[BskSession],
    instances: &[String],
    started: Option<&str>,
    hint: Option<&str>,
    shared: bool,
) -> Choice {
    let live: Vec<&BskSession> = sessions
        .iter()
        .filter(|s| instances.contains(&s.browser))
        .collect();
    let has = |id: Option<&str>| id.is_some_and(|id| live.iter().any(|s| s.id == id));
    if has(started) {
        return Choice::Session(started.unwrap_or_default().to_string(), "started here");
    }
    if has(hint) {
        return Choice::Session(hint.unwrap_or_default().to_string(), "the agent's");
    }
    match live.as_slice() {
        [] => Choice::NoSession,
        [one] if !shared => Choice::Session(one.id.clone(), "the only one"),
        many => Choice::Unsure(many.len()),
    }
}

// ---------- what the UI thread hands over ----------

/// Everything a `bview` frame needs from the app, taken on the UI thread
/// (the bsk calls run on their own thread).
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub uid: u64,
    pub bin: Option<PathBuf>,
    /// The tab's Chrome profile (None: no browser set).
    pub profile: Option<ChromeProfile>,
    pub enabled_profiles: usize,
    pub this_enabled: bool,
    /// The agent's transcript, for the session it used.
    pub transcript: Option<PathBuf>,
    /// The tab's more profiles, each with whether its extension is on.
    pub also: Vec<(ChromeProfile, bool)>,
    /// Labels the user linked to profiles (`[browser] links`).
    pub links: crate::tab_browser::Links,
    /// Another tab is bound to the same profile.
    pub shared: bool,
    /// Where pictures are written for a moment.
    pub tmp: PathBuf,
    /// `sips` (macOS) to make JPEGs; None sends the PNG.
    pub sips: Option<PathBuf>,
}

/// Sessions started from the view, by tab uid.
fn started() -> &'static Mutex<HashMap<u64, String>> {
    static S: std::sync::OnceLock<Mutex<HashMap<u64, String>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

fn bsk(ctx: &Ctx, args: &[&str], timeout: Duration) -> Result<String, String> {
    let bin = ctx
        .bin
        .as_deref()
        .ok_or("BrowserSkill (bsk) is not installed")?;
    match browser::run_bsk(bin, args, timeout) {
        Ok((0, out)) => Ok(out),
        Ok((code, out)) => {
            Err(bsk_error(&out).unwrap_or_else(|| format!("bsk {} failed ({code})", args[0])))
        }
        Err(browser::RunErr::Timeout) => Err(format!("bsk {} took too long", args[0])),
        Err(browser::RunErr::Spawn(e)) => Err(format!("could not run bsk: {e}")),
    }
}

/// The message of a bsk JSON error (`{error: {message}}`, `{error: "..."}`,
/// `{message}`).
fn bsk_error(out: &str) -> Option<String> {
    let v: Value = serde_json::from_str(out.trim()).ok()?;
    let e = v.get("error").unwrap_or(&v);
    let m = e
        .get("message")
        .or_else(|| e.get("reason"))
        .and_then(Value::as_str)
        .or_else(|| e.as_str())?;
    Some(crate::sessions::snippet(m, 160))
}

/// The tab's own browsers (bsk instances) now.
fn instances(ctx: &Ctx) -> Result<Vec<Browser>, String> {
    let p = ctx.profile.as_ref().ok_or("no browser set")?;
    let all = browser::parse_browsers(&bsk(ctx, &["browsers", "--json"], CALL_TIMEOUT)?);
    let m = |p: &ChromeProfile, on: bool| {
        crate::tab_browser::match_browsers(&all, p, ctx.enabled_profiles, on, &ctx.links).0
    };
    let mut out = m(p, ctx.this_enabled);
    for (q, on) in &ctx.also {
        for b in m(q, *on) {
            if !out.iter().any(|x| x.instance_id == b.instance_id) {
                out.push(b);
            }
        }
    }
    Ok(out)
}

/// The tab's session, or a `bview` reply saying why there is none.
fn resolve(ctx: &Ctx) -> Result<String, Value> {
    let none = |state: &str, say: String, can_start: bool| json!({"ok": true, "state": state, "say": say, "can_start": can_start});
    let Some(p) = ctx.profile.as_ref() else {
        return Err(none(
            "no_browser",
            "No browser is set for this tab.".into(),
            false,
        ));
    };
    if ctx.bin.is_none() {
        return Err(none(
            "no_bsk",
            "BrowserSkill (bsk) is not installed.".into(),
            false,
        ));
    }
    let mine = instances(ctx).map_err(|e| none("error", e, false))?;
    if mine.is_empty() {
        return Err(none(
            "not_connected",
            format!("{} is not connected to BrowserSkill (or its label in the extension popup does not name the profile).", p.display()),
            false,
        ));
    }
    let ids: Vec<String> = mine.iter().map(|b| b.instance_id.clone()).collect();
    let sessions = parse_sessions(
        &bsk(ctx, &["session", "list", "--json"], CALL_TIMEOUT)
            .map_err(|e| none("error", e, false))?,
    );
    let hint = ctx
        .transcript
        .as_deref()
        .and_then(|t| crate::tab_browser::read_tail(t, 512 * 1024))
        .and_then(|t| last_session(&t));
    let st = started()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&ctx.uid)
        .cloned();
    match choose(&sessions, &ids, st.as_deref(), hint.as_deref(), ctx.shared) {
        Choice::Session(id, _) => Ok(id),
        Choice::NoSession => Err(none(
            "none",
            "No active session: the agent hasn't opened the browser.".into(),
            mine.len() == 1,
        )),
        Choice::Unsure(n) => Err(none(
            "unsure",
            format!(
                "{n} sessions are open on this browser and none is known to be this tab's agent's."
            ),
            false,
        )),
    }
}

// ---------- the capture loops ----------

struct Viewer {
    client: u64,
    uid: u64,
    out: Arc<Outbox>,
    keep: Instant,
}

#[derive(Default)]
struct Shared {
    viewers: Vec<Viewer>,
    /// The newest URL and title (sent to a viewer that joins).
    meta: Option<Value>,
    /// Bring the next capture forward (an action happened).
    soon: bool,
    /// The viewport in CSS pixels, once asked for.
    viewport: Option<(f64, f64)>,
}

struct Stream {
    session: String,
    ctx: Ctx,
    s: Mutex<Shared>,
    cv: Condvar,
}

impl Stream {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.s.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn streams() -> &'static Mutex<HashMap<String, Arc<Stream>>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, Arc<Stream>>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

fn all_streams() -> Vec<Arc<Stream>> {
    streams()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect()
}

/// The stream `client` watches for tab `uid`.
fn watched(client: u64, uid: u64) -> Option<Arc<Stream>> {
    all_streams().into_iter().find(|s| {
        s.lock()
            .viewers
            .iter()
            .any(|v| v.client == client && v.uid == uid)
    })
}

/// A capture loop runs for `session` (tests; the tests that use it are Unix only).
#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
pub fn streaming(session: &str) -> bool {
    streams()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(session)
}

/// Stop watching tab `uid` (every tab with None) for `client`.
pub fn unwatch(client: u64, uid: Option<u64>) {
    for s in all_streams() {
        let mut g = s.lock();
        let n = g.viewers.len();
        g.viewers
            .retain(|v| !(v.client == client && uid.is_none_or(|u| u == v.uid)));
        if g.viewers.len() != n {
            s.cv.notify_all();
        }
    }
}

/// Watch `session` for tab `uid`: joins its loop, or starts one.
fn watch(ctx: &Ctx, session: &str, client: u64, out: &Arc<Outbox>) {
    let uid = ctx.uid;
    // A view shows one session per tab.
    for s in all_streams() {
        if s.session != session {
            let mut g = s.lock();
            g.viewers.retain(|v| !(v.client == client && v.uid == uid));
        }
    }
    let mut map = streams().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = map.get(session) {
        let mut g = s.lock();
        if let Some(v) = g
            .viewers
            .iter_mut()
            .find(|v| v.client == client && v.uid == uid)
        {
            v.keep = Instant::now();
        } else {
            g.viewers.push(Viewer {
                client,
                uid,
                out: Arc::clone(out),
                keep: Instant::now(),
            });
        }
        if let Some(m) = &g.meta {
            let mut m = m.clone();
            m["tab"] = json!(uid);
            out.text(m.to_string());
        }
        g.soon = true;
        s.cv.notify_all();
        return;
    }
    let s = Arc::new(Stream {
        session: session.to_string(),
        ctx: ctx.clone(),
        s: Mutex::new(Shared {
            viewers: vec![Viewer {
                client,
                uid,
                out: Arc::clone(out),
                keep: Instant::now(),
            }],
            ..Default::default()
        }),
        cv: Condvar::new(),
    });
    map.insert(session.to_string(), Arc::clone(&s));
    drop(map);
    crate::log::info(&format!("bview: watching session {session} (tab {uid})"));
    let _ = std::thread::Builder::new()
        .name("bview".into())
        .spawn(move || capture_loop(&s));
}

/// Send `v` (a `bview` frame without its tab) to every viewer.
fn tell(s: &Stream, v: &Value) {
    for w in &s.lock().viewers {
        let mut m = v.clone();
        m["t"] = json!("bview");
        m["tab"] = json!(w.uid);
        w.out.text(m.to_string());
    }
}

/// Drop viewers that left; false when none is left (the loop is then
/// taken out of the registry, unless one joined meanwhile).
fn prune(s: &Arc<Stream>) -> bool {
    let mut map = streams().lock().unwrap_or_else(|e| e.into_inner());
    let mut g = s.lock();
    g.viewers
        .retain(|v| !v.out.is_closed() && v.keep.elapsed() < KEEP_FOR);
    if g.viewers.is_empty() {
        if map.get(&s.session).is_some_and(|x| Arc::ptr_eq(x, s)) {
            map.remove(&s.session);
        }
        return false;
    }
    true
}

fn capture_loop(s: &Arc<Stream>) {
    let mut last_meta: Option<Instant> = None;
    let mut fails = 0u32;
    while prune(s) {
        let t0 = Instant::now();
        if last_meta.is_none_or(|t| t.elapsed() >= META_EVERY) {
            last_meta = Some(Instant::now());
            if let Ok(out) = bsk(
                &s.ctx,
                &[
                    "tab",
                    "list",
                    "--session",
                    &s.session,
                    "--scope",
                    "agent",
                    "--json",
                ],
                CALL_TIMEOUT,
            ) {
                let tabs = parse_tabs(&out);
                if let Some(t) = current_tab(&tabs) {
                    let m = json!({"t": "bview", "op": "meta", "url": t.url, "title": t.title, "tabs": tabs.len()});
                    let changed = s.lock().meta.as_ref() != Some(&m);
                    if changed {
                        s.lock().meta = Some(m.clone());
                        tell(s, &m);
                    }
                }
            }
        }
        match capture(&s.ctx, &s.session) {
            Ok(pic) => {
                fails = 0;
                for v in &s.lock().viewers {
                    v.out
                        .picture(crate::web::ws::frame(KIND_PICTURE, v.uid, &pic));
                }
            }
            Err(e) => {
                fails += 1;
                // Gone (the agent stopped it): say so and stop.
                let live = bsk(&s.ctx, &["session", "list", "--json"], CALL_TIMEOUT)
                    .map(|o| parse_sessions(&o).iter().any(|x| x.id == s.session));
                if live == Ok(false) {
                    crate::log::info(&format!("bview: session {} ended", s.session));
                    tell(s, &json!({"op": "ended", "say": "The session ended."}));
                    let mut map = streams().lock().unwrap_or_else(|e| e.into_inner());
                    if map.get(&s.session).is_some_and(|x| Arc::ptr_eq(x, s)) {
                        map.remove(&s.session);
                    }
                    started()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .retain(|_, v| *v != s.session);
                    return;
                }
                if fails == 1 || fails.is_multiple_of(10) {
                    tell(s, &json!({"op": "error", "error": e}));
                }
            }
        }
        // Twice the capture's time (no overlap, at most ~1.5 a second),
        // slower while it keeps failing; sooner after an action.
        let took = t0.elapsed();
        let mut period = (took * 2).clamp(MIN_PERIOD, MAX_PERIOD);
        if fails > 0 {
            period = MAX_PERIOD;
        }
        let until = Instant::now() + period.saturating_sub(took);
        let mut g = s.lock();
        loop {
            if g.soon {
                g.soon = false;
                drop(g);
                std::thread::sleep(AFTER_ACTION);
                break;
            }
            let now = Instant::now();
            if now >= until || g.viewers.is_empty() {
                break;
            }
            g =
                s.cv.wait_timeout(g, until - now)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
        }
    }
    crate::log::info(&format!("bview: stopped watching session {}", s.session));
}

/// One viewport picture: JPEG when that is smaller (and scaled down when
/// very wide), else the PNG.
fn capture(ctx: &Ctx, session: &str) -> Result<Vec<u8>, String> {
    let _ = std::fs::create_dir_all(&ctx.tmp);
    let tag: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(40)
        .collect();
    let png = ctx.tmp.join(format!("bview-{tag}.png"));
    let jpg = ctx.tmp.join(format!("bview-{tag}.jpg"));
    let _ = std::fs::remove_file(&png);
    let p = png.to_string_lossy().into_owned();
    let out = bsk(
        ctx,
        &["screenshot", "--session", session, "--json", "--out", &p],
        CALL_TIMEOUT,
    )?;
    let bytes = std::fs::read(&png)
        .map_err(|_| bsk_error(&out).unwrap_or_else(|| "no picture came back".into()))?;
    let _ = std::fs::remove_file(&png);
    if image_size(&bytes).is_none() {
        return Err("the picture is not a PNG".into());
    }
    Ok(match ctx.sips.as_deref() {
        Some(sips) => to_jpeg(sips, &bytes, &png, &jpg).unwrap_or(bytes),
        None => bytes,
    })
}

fn to_jpeg(sips: &Path, png_bytes: &[u8], png: &Path, jpg: &Path) -> Option<Vec<u8>> {
    let (w, _) = image_size(png_bytes)?;
    std::fs::write(png, png_bytes).ok()?;
    let mut args: Vec<String> = ["-s", "format", "jpeg", "-s", "formatOptions", "70"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if w > MAX_WIDTH {
        args.extend(["--resampleWidth".into(), MAX_WIDTH.to_string()]);
    }
    args.extend([
        png.to_string_lossy().into_owned(),
        "--out".into(),
        jpg.to_string_lossy().into_owned(),
    ]);
    let ok = std::process::Command::new(sips)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let _ = std::fs::remove_file(png);
    let out = ok.then(|| std::fs::read(jpg).ok()).flatten();
    let _ = std::fs::remove_file(jpg);
    let out = out.filter(|j| image_size(j).is_some())?;
    // A scaled picture is always taken; else only a smaller one.
    (w > MAX_WIDTH || out.len() < png_bytes.len()).then_some(out)
}

// ---------- actions ----------

/// A URL typed in the view: http(s) only; a bare host gets https.
pub fn clean_url(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control) {
        return None;
    }
    let low = s.to_ascii_lowercase();
    if low.starts_with("http://") || low.starts_with("https://") {
        return Some(s.to_string());
    }
    if low.contains("://")
        || low.contains(char::is_whitespace)
        || !s.contains('.') && !low.starts_with("localhost")
    {
        return None;
    }
    let host = s.split(['/', '?', '#']).next().unwrap_or("");
    if host.contains(':')
        && !host
            .rsplit(':')
            .next()
            .unwrap_or("")
            .chars()
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(format!("https://{s}"))
}

/// A key for `bsk press`: a name or a combo (`Enter`, `Ctrl+A`).
pub fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= 24 && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '+')
}

fn frac(v: &Value, k: &str) -> Option<f64> {
    v[k].as_f64()
        .filter(|x| x.is_finite() && (0.0..=1.0).contains(x))
}

/// The page script of a click at (fx, fy) (fractions of the viewport):
/// the element there is marked for a real `bsk click` when its centre is
/// that element too (a button, a link); else the click is dispatched at
/// the point itself (a big container, a canvas).
pub fn click_script(fx: f64, fy: f64, mark: &str) -> String {
    format!(
        r#"(() => {{
  for (const o of document.querySelectorAll('[data-godterm-hit]')) o.removeAttribute('data-godterm-hit');
  const x = {fx} * innerWidth, y = {fy} * innerHeight;
  const e = document.elementFromPoint(x, y);
  if (!e) return {{ mode: 'none' }};
  const r = e.getBoundingClientRect();
  const c = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
  const small = r.width * r.height < innerWidth * innerHeight / 6;
  if (small && c && (c === e || e.contains(c))) {{ e.setAttribute('data-godterm-hit', '{mark}'); return {{ mode: 'native', tag: e.tagName, w: innerWidth, h: innerHeight }}; }}
  const o = {{ bubbles: true, cancelable: true, composed: true, clientX: x, clientY: y, view: window, button: 0 }};
  e.dispatchEvent(new PointerEvent('pointerdown', o)); e.dispatchEvent(new MouseEvent('mousedown', o));
  if (e.focus) e.focus();
  e.dispatchEvent(new PointerEvent('pointerup', o)); e.dispatchEvent(new MouseEvent('mouseup', o));
  e.dispatchEvent(new MouseEvent('click', o));
  return {{ mode: 'synthetic', tag: e.tagName, w: innerWidth, h: innerHeight }};
}})()"#
    )
}

/// The first object with `key` in a JSON answer.
fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(o) => o
            .get(key)
            .or_else(|| o.values().find_map(|x| find_key(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

/// Run one action on `session`. Ok: a word for the page.
fn act(ctx: &Ctx, session: &str, v: &Value, stream: &Stream) -> Result<String, String> {
    let what = v["do"].as_str().unwrap_or("");
    let s = session;
    let t = "10s";
    let run = |args: &[&str]| bsk(ctx, args, Duration::from_secs(35));
    match what {
        "back" => {
            run(&["navigate-back", "--session", s, "--json", "--timeout", t]).map(|_| "Back".into())
        }
        "forward" => run(&["navigate-forward", "--session", s, "--json", "--timeout", t])
            .map(|_| "Forward".into()),
        "reload" => {
            run(&["reload", "--session", s, "--json", "--timeout", t]).map(|_| "Reloaded".into())
        }
        "navigate" => {
            let url = clean_url(v["url"].as_str().unwrap_or(""))
                .ok_or("Not a web address (http or https)")?;
            run(&[
                "navigate",
                &url,
                "--session",
                s,
                "--json",
                "--timeout",
                "30s",
            ])
            .map(|_| "Opened".into())
        }
        "click" => {
            let (Some(fx), Some(fy)) = (frac(v, "x"), frac(v, "y")) else {
                return Err("bad point".into());
            };
            let mark = format!(
                "{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            );
            let out = run(&[
                "evaluate",
                &click_script(fx, fy, &mark),
                "--session",
                s,
                "--json",
            ])?;
            let r: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
            if let (Some(w), Some(h)) = (
                find_key(&r, "w").and_then(Value::as_f64),
                find_key(&r, "h").and_then(Value::as_f64),
            ) {
                stream.lock().viewport = Some((w, h));
            }
            match find_key(&r, "mode").and_then(Value::as_str) {
                Some("native") => {
                    let sel = format!("[data-godterm-hit=\"{mark}\"]");
                    run(&[
                        "click",
                        "--selector",
                        &sel,
                        "--session",
                        s,
                        "--json",
                        "--timeout",
                        t,
                    ])
                    .map(|_| "Clicked".into())
                }
                Some("synthetic") => Ok("Clicked".into()),
                Some("none") => Err("Nothing there to click".into()),
                _ => Err(bsk_error(&out).unwrap_or_else(|| "The click did not go through".into())),
            }
        }
        "wheel" => {
            // Fractions of the picture, in page pixels.
            let dy = v["dy"]
                .as_f64()
                .filter(|x| x.is_finite())
                .unwrap_or(0.0)
                .clamp(-5.0, 5.0);
            let dx = v["dx"]
                .as_f64()
                .filter(|x| x.is_finite())
                .unwrap_or(0.0)
                .clamp(-5.0, 5.0);
            let (w, h) = stream.lock().viewport.unwrap_or((1280.0, 800.0));
            let (py, px) = ((dy * h).round() as i64, (dx * w).round() as i64);
            if py == 0 && px == 0 {
                return Ok(String::new());
            }
            let (py, px) = (py.to_string(), px.to_string());
            run(&[
                "wheel",
                "--delta-y",
                &py,
                "--delta-x",
                &px,
                "--session",
                s,
                "--json",
                "--timeout",
                t,
            ])
            .map(|_| String::new())
        }
        "type" => {
            let text = v["text"].as_str().unwrap_or("");
            if text.is_empty() || text.len() > 2000 {
                return Err("Nothing to type".into());
            }
            let js = format!(
                "(() => {{ const e = document.activeElement; if (!e || e === document.body) return {{ typed: false }}; return {{ typed: document.execCommand('insertText', false, {}) }}; }})()",
                Value::String(text.to_string())
            );
            let out = run(&["evaluate", &js, "--session", s, "--json"])?;
            let r: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
            match find_key(&r, "typed").and_then(Value::as_bool) {
                Some(true) => Ok("Typed".into()),
                _ => Err("Click a text field in the page first".into()),
            }
        }
        "key" => {
            let k = v["key"].as_str().unwrap_or("");
            if !valid_key(k) {
                return Err("bad key".into());
            }
            run(&["press", k, "--session", s, "--json", "--timeout", t]).map(|_| k.to_string())
        }
        _ => Err(format!("unknown action {what:?}")),
    }
}

// ---------- a frame ----------

/// Answer one `bview` frame (on its own thread): `open` attaches the view
/// to the tab's session and starts the pictures, `keep` says the viewer
/// still watches, `close` stops, `act` runs an action on the watched
/// session, `start` starts a session on the tab's browser (the user's
/// press of the Start button).
pub fn handle(ctx: &Ctx, client: u64, out: &Arc<Outbox>, v: &Value) {
    let op = v["op"].as_str().unwrap_or("");
    let reply = |mut r: Value| {
        r["t"] = json!("bview");
        r["id"] = v["id"].clone();
        r["tab"] = json!(ctx.uid);
        r["op"] = json!(op);
        out.text(r.to_string());
    };
    match op {
        "open" => match resolve(ctx) {
            Ok(session) => {
                watch(ctx, &session, client, out);
                reply(json!({"ok": true, "state": "live", "session": short(&session)}));
            }
            Err(r) => {
                unwatch(client, Some(ctx.uid));
                reply(r);
            }
        },
        "keep" => {
            let live = watched(client, ctx.uid).is_some_and(|s| {
                let mut g = s.lock();
                g.viewers
                    .iter_mut()
                    .filter(|w| w.client == client && w.uid == ctx.uid)
                    .for_each(|w| w.keep = Instant::now());
                true
            });
            if v["id"].is_string() || v["id"].is_number() {
                reply(json!({"ok": live}));
            }
        }
        "close" => {
            unwatch(client, Some(ctx.uid));
            if v["id"].is_string() || v["id"].is_number() {
                reply(json!({"ok": true}));
            }
        }
        "act" => {
            let Some(s) = watched(client, ctx.uid) else {
                return reply(
                    json!({"ok": false, "error": "The view is not attached to a session"}),
                );
            };
            let r = act(ctx, &s.session, v, &s);
            {
                let mut g = s.lock();
                g.soon = true;
                s.cv.notify_all();
            }
            match r {
                Ok(say) => reply(json!({"ok": true, "say": say})),
                Err(e) => reply(json!({"ok": false, "error": e})),
            }
        }
        "start" => {
            if let Ok(id) = resolve(ctx) {
                watch(ctx, &id, client, out);
                return reply(json!({"ok": true, "state": "live", "session": short(&id)}));
            }
            let r = (|| -> Result<String, String> {
                let mine = instances(ctx)?;
                let [b] = mine.as_slice() else {
                    return Err("The tab's browser is not connected, or not only one is".into());
                };
                let out = bsk(
                    ctx,
                    &[
                        "session",
                        "start",
                        "--browser",
                        &b.instance_id,
                        "--name",
                        "GodTerm live view",
                        "--json",
                    ],
                    Duration::from_secs(90),
                )?;
                let v: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
                let id = find_key(&v, "session_id")
                    .or_else(|| find_key(&v, "id"))
                    .and_then(Value::as_str)
                    .filter(|id| valid_id(id))
                    .ok_or("bsk did not say which session it started")?
                    .to_string();
                crate::log::info(&format!(
                    "bview: the user started session {id} on {} (tab {})",
                    b.instance_id, ctx.uid
                ));
                started()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(ctx.uid, id.clone());
                Ok(id)
            })();
            match r {
                Ok(id) => {
                    watch(ctx, &id, client, out);
                    reply(json!({"ok": true, "state": "live", "session": short(&id)}));
                }
                Err(e) => reply(json!({"ok": false, "error": e})),
            }
        }
        other => reply(json!({"ok": false, "error": format!("unknown op {other:?}")})),
    }
}

/// The first 8 characters of a session id (for the page; the id itself
/// never needs to leave GodTerm).
fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

// ---------- the UI thread's side ----------

impl crate::app::App {
    /// What a `bview` frame for tab `uid` needs to know.
    pub fn bview_ctx(&self, uid: u64) -> Ctx {
        use crate::tab_browser::{button_state, ext_state, BtnState, ExtState};
        let env = self.browser_env_pub();
        let bin = env.as_ref().and_then(|e| browser::find_bsk(&e.bin_dirs));
        let dir = self.tab_browser_dir(uid);
        let mut profiles = self.browser_profiles();
        if profiles.is_empty() {
            if let Some(e) = &env {
                profiles = browser::chrome_extensions(&e.chrome_root);
            }
        }
        let profile = dir.as_ref().map(|d| {
            profiles
                .iter()
                .find(|p| &p.profile.dir == d)
                .map(|p| p.profile.clone())
                .unwrap_or(ChromeProfile {
                    dir: d.clone(),
                    name: d.clone(),
                    ..Default::default()
                })
        });
        let found = self.find_tab(uid);
        let dirs = self.tab_browser_dirs(uid);
        let also: Vec<(ChromeProfile, bool)> = dirs
            .iter()
            .skip(1)
            .map(|d| {
                let q = profiles
                    .iter()
                    .find(|p| &p.profile.dir == d)
                    .map(|p| p.profile.clone())
                    .unwrap_or(ChromeProfile {
                        dir: d.clone(),
                        name: d.clone(),
                        ..Default::default()
                    });
                (q, button_state(Some(d), &profiles) == BtnState::Ready)
            })
            .collect();
        let shared = self.panes.iter().flat_map(|p| p.tabs.iter()).any(|t| {
            t.uid != uid
                && t.browser
                    .iter()
                    .chain(t.browser_also.iter())
                    .any(|d| dirs.contains(d))
        });
        Ctx {
            uid,
            bin,
            also,
            links: self.cfg.browser.links.clone(),
            enabled_profiles: profiles
                .iter()
                .filter(|p| ext_state(p) == ExtState::Enabled)
                .count(),
            this_enabled: dir.is_some()
                && button_state(dir.as_deref(), &profiles) == BtnState::Ready,
            profile,
            transcript: found
                .and_then(|(s, t)| self.transcript_of(s, t))
                .map(|(p, _)| p),
            shared,
            tmp: std::env::temp_dir().join(format!("godterm-bview-{}", std::process::id())),
            sips: Some(PathBuf::from("/usr/bin/sips"))
                .filter(|p| cfg!(target_os = "macos") && p.is_file()),
        }
    }

    /// A `bview` frame from a browser: answered on its own thread.
    pub fn web_bview(&mut self, client: u64, v: Value) {
        let Some(c) = self.web.as_ref().and_then(|w| w.hub.client(client)) else {
            return;
        };
        let uid = v["tab"].as_u64().or_else(|| {
            v["tab"]
                .as_str()
                .and_then(|t| t.trim_start_matches('t').parse().ok())
        });
        let Some(uid) = uid.filter(|u| self.find_tab(*u).is_some()) else {
            c.out.text(
                json!({"t": "bview", "id": v["id"], "tab": v["tab"], "op": v["op"], "ok": false, "error": "no such tab"})
                    .to_string(),
            );
            return;
        };
        let ctx = self.bview_ctx(uid);
        let out = Arc::clone(&c.out);
        let _ = std::thread::Builder::new()
            .name("bview-req".into())
            .spawn(move || handle(&ctx, client, &out, &v));
    }
}
