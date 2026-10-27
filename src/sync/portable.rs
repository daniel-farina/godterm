//! Paths across machines and the config split (docs/SYNC.md 2.3, 2.4).
//!
//! A path under the home is stored as `~/rest`; one under a folder named
//! in `[sync.paths]` as `@name/rest`; anything else as is. config.toml is
//! split into portable keys (pushed, newest change per key wins) and
//! machine local ones (never pushed, never applied). Pulled values are
//! data: keys that run programs or open the machine up are refused.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item};

/// Maps paths between this machine and portable form.
#[derive(Debug, Clone, PartialEq)]
pub struct PathMap {
    pub home: PathBuf,
    /// (name, folder), longest folder first.
    pub named: Vec<(String, PathBuf)>,
}

impl PathMap {
    pub fn new(home: &Path, named: &BTreeMap<String, String>) -> PathMap {
        let mut v: Vec<(String, PathBuf)> = named
            .iter()
            .map(|(k, p)| (k.clone(), PathBuf::from(p)))
            .collect();
        v.sort_by_key(|(_, p)| std::cmp::Reverse(p.as_os_str().len()));
        PathMap {
            home: home.to_path_buf(),
            named: v,
        }
    }

    /// The portable form of an absolute path.
    pub fn to_portable(&self, p: &Path) -> String {
        for (name, dir) in &self.named {
            if let Ok(rest) = p.strip_prefix(dir) {
                return join_rest(&format!("@{name}"), rest);
            }
        }
        if let Ok(rest) = p.strip_prefix(&self.home) {
            return join_rest("~", rest);
        }
        p.to_string_lossy().into_owned()
    }

    /// This machine's path for a portable one (None: a `@name` this
    /// machine has no folder for).
    pub fn to_local(&self, s: &str) -> Option<PathBuf> {
        if s == "~" {
            return Some(self.home.clone());
        }
        if let Some(r) = s.strip_prefix("~/") {
            return Some(self.home.join(native_rest(r)));
        }
        if let Some(r) = s.strip_prefix('@') {
            let (name, rest) = r.split_once('/').unwrap_or((r, ""));
            let dir = &self.named.iter().find(|(n, _)| n == name)?.1;
            return Some(if rest.is_empty() {
                dir.clone()
            } else {
                dir.join(native_rest(rest))
            });
        }
        Some(PathBuf::from(s))
    }

    /// A path written in config (maybe already `~/...`) made portable.
    pub fn portable_text(&self, s: &str) -> String {
        if s.starts_with('~') || s.starts_with('@') || !is_abs_text(s) {
            return s.to_string();
        }
        self.to_portable(Path::new(s))
    }

    /// A portable path as config text on this machine: `~/...` stays.
    pub fn local_text(&self, s: &str) -> String {
        if s.starts_with('~') || !s.starts_with('@') {
            return s.to_string();
        }
        self.to_local(s)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.to_string())
    }

    /// Claude's project folder name for a portable cwd on this machine.
    #[cfg(test)]
    pub fn project_dir_name(&self, portable_cwd: &str) -> Option<String> {
        Some(crate::state::encode_project_dir(
            &self.to_local(portable_cwd)?,
        ))
    }
}

/// A portable `a/b` tail in this platform's separators (Windows paths
/// stay `C:\x\a\b`, not `C:\x\a/b`, so they compare equal).
fn native_rest(r: &str) -> String {
    if cfg!(windows) {
        r.replace('/', "\\")
    } else {
        r.to_string()
    }
}

/// Whether config text is an absolute path on any platform: `/x`,
/// `C:\x`, `C:/x` or a UNC path.
fn is_abs_text(s: &str) -> bool {
    let b = s.as_bytes();
    s.starts_with('/')
        || s.starts_with("\\\\")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/'))
}

fn join_rest(head: &str, rest: &Path) -> String {
    // Portable form always uses `/`, whatever the platform's separator.
    let mut r = rest.to_string_lossy().into_owned();
    if cfg!(windows) {
        r = r.replace('\\', "/");
    }
    if r.is_empty() {
        head.to_string()
    } else {
        format!("{head}/{r}")
    }
}

/// A short stable key for a portable path: a slug plus 8 hex of its hash.
pub fn path_key(portable: &str) -> String {
    let slug: String = portable
        .trim_start_matches(['~', '@', '/'])
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    let slug = crate::paths::head(slug, 48).trim_end_matches('-');
    let h = super::sha_hex(portable.as_bytes());
    if slug.is_empty() {
        format!("home-{}", &h[..8])
    } else {
        format!("{slug}-{}", &h[..8])
    }
}

/// Not a config key: the order of the `[[account]]` tables.
pub const ACCOUNT_ORDER: &str = "account_order";

/// Machine local config keys, besides `[sync] machine_local`.
const LOCAL: &[&str] = &[
    "web",
    "supervisor",
    "update",
    "sync",
    "remember_window",
    "refresh_secs",
    "pty_read_buffer_kb",
    "writer_queue_kb",
    "voice.model",
    "voice.wake_model",
    "voice.partial_model",
    "voice.whisper_server",
    "voice.whisper_cli",
    "voice.ffmpeg",
    "voice.device",
    "voice.output_device",
    "voice.kokoro_model",
    "voice.kokoro_voices",
    "voice.espeak",
    "voice.speaker_model",
    "voice.local",
];

/// Keys holding paths, mapped both ways.
fn path_valued(key: &str) -> bool {
    key == "trusted_dirs"
        || key == "new_tab_base"
        || (key.starts_with("account.")
            && (key.ends_with(".new_tab_base") || key.ends_with(".cwd")))
}

fn prefix_match(key: &str, pat: &str) -> bool {
    let pat = pat.trim_end_matches(".*");
    key == pat || key.starts_with(&format!("{pat}."))
}

/// Whether a config key stays on this machine.
pub fn is_local_key(key: &str, extra: &[String]) -> bool {
    let last = key.rsplit('.').next().unwrap_or(key);
    last.ends_with("_bin")
        || (key.starts_with("account.") && last == "config_dir")
        || LOCAL.iter().any(|p| prefix_match(key, p))
        || extra.iter().any(|p| prefix_match(key, p))
}

/// Why a pulled value may not be applied here, if so: a key that would run
/// a program, open the machine up, or pass shell syntax to a CLI.
pub fn unsafe_pulled(key: &str, v: &Value) -> Option<String> {
    if is_local_key(key, &[]) {
        return Some(format!("{key} is machine local"));
    }
    if key.starts_with("account.") && (key.ends_with(".args") || key.ends_with(".harness")) {
        let text = v.to_string();
        if text.chars().any(|c| ";|&$`<>(){}\\\n".contains(c)) {
            return Some(format!("{key} has shell syntax"));
        }
    }
    if key == "pass_env" {
        return Some("pass_env is machine local (it hands environment to agents)".into());
    }
    None
}

/// The portable keys of a config.toml text, flattened: `voice.tts_speed`,
/// `account.<name>.label`, paths in portable form.
pub fn flatten(
    text: &str,
    map: &PathMap,
    extra: &[String],
) -> anyhow::Result<BTreeMap<String, Value>> {
    let v: toml::Value = toml::from_str(text)?;
    let mut out = BTreeMap::new();
    let Some(t) = v.as_table() else {
        return Ok(out);
    };
    for (k, val) in t {
        if k == "account" {
            let mut order = vec![];
            for a in val.as_array().into_iter().flatten() {
                let Some(at) = a.as_table() else { continue };
                let Some(name) = at.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                for (f, fv) in at {
                    if f == "name" {
                        continue;
                    }
                    put(&mut out, &format!("account.{name}.{f}"), fv, map, extra);
                }
                // Present even with no other key, so a new account arrives.
                out.entry(format!("account.{name}.name"))
                    .or_insert(Value::String(name.to_string()));
                order.push(Value::String(name.to_string()));
            }
            // The order matters (the grid shows the first ones).
            out.insert(ACCOUNT_ORDER.to_string(), Value::Array(order));
            continue;
        }
        walk(&mut out, k, val, map, extra);
    }
    Ok(out)
}

fn walk(
    out: &mut BTreeMap<String, Value>,
    key: &str,
    v: &toml::Value,
    map: &PathMap,
    extra: &[String],
) {
    if is_local_key(key, extra) {
        return;
    }
    match v {
        toml::Value::Table(t) => {
            for (k, x) in t {
                walk(out, &format!("{key}.{k}"), x, map, extra);
            }
        }
        _ => put(out, key, v, map, extra),
    }
}

fn put(
    out: &mut BTreeMap<String, Value>,
    key: &str,
    v: &toml::Value,
    map: &PathMap,
    extra: &[String],
) {
    if is_local_key(key, extra) {
        return;
    }
    let mut j = serde_json::to_value(v).unwrap_or(Value::Null);
    if path_valued(key) {
        j = map_paths(&j, |s| map.portable_text(s));
    }
    out.insert(key.to_string(), j);
}

fn map_paths(v: &Value, f: impl Fn(&str) -> String + Copy) -> Value {
    match v {
        Value::String(s) => Value::String(f(s)),
        Value::Array(a) => Value::Array(a.iter().map(|x| map_paths(x, f)).collect()),
        other => other.clone(),
    }
}

fn to_toml(v: &Value) -> Option<toml_edit::Value> {
    Some(match v {
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into()
            } else {
                n.as_f64()?.into()
            }
        }
        Value::String(s) => s.as_str().into(),
        Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for x in a {
                arr.push(to_toml(x)?);
            }
            toml_edit::Value::Array(arr)
        }
        Value::Object(o) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, x) in o {
                t.insert(k, to_toml(x)?);
            }
            toml_edit::Value::InlineTable(t)
        }
        Value::Null => return None,
    })
}

/// What applying pulled keys did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Applied {
    pub changed: Vec<String>,
    pub refused: Vec<String>,
    /// The keys behind `refused`.
    pub refused_keys: Vec<String>,
}

/// Apply pulled portable keys onto a local config.toml text (comments and
/// local keys kept): `set` writes values, `removed` deletes keys (back to
/// their default). The result must still parse as a Config.
pub fn apply(
    text: &str,
    set: &BTreeMap<String, Value>,
    removed: &[String],
    map: &PathMap,
) -> anyhow::Result<(String, Applied)> {
    let mut doc: DocumentMut = text.parse()?;
    let mut ap = Applied::default();
    for (k, v) in set {
        if k == ACCOUNT_ORDER {
            continue;
        }
        if let Some(why) = unsafe_pulled(k, v) {
            ap.refused.push(why);
            ap.refused_keys.push(k.clone());
            continue;
        }
        let v = if path_valued(k) {
            map_paths(v, |s| map.local_text(s))
        } else {
            v.clone()
        };
        let Some(tv) = to_toml(&v) else { continue };
        if set_key(&mut doc, k, tv)? {
            ap.changed.push(k.clone());
        }
    }
    for k in removed {
        if unsafe_pulled(k, &Value::Null).is_some() || k.ends_with(".name") {
            continue;
        }
        if remove_key(&mut doc, k) {
            ap.changed.push(k.clone());
        }
    }
    if let Some(order) = set.get(ACCOUNT_ORDER).and_then(Value::as_array) {
        let names: Vec<&str> = order.iter().filter_map(Value::as_str).collect();
        if reorder_accounts(&mut doc, &names) {
            ap.changed.push(ACCOUNT_ORDER.into());
        }
    }
    let out = doc.to_string();
    crate::config::Config::parse(&out)?;
    Ok((out, ap))
}

/// Put the named accounts first, in that order (others keep theirs).
fn reorder_accounts(doc: &mut DocumentMut, names: &[&str]) -> bool {
    let Some(arr) = doc
        .get_mut("account")
        .and_then(Item::as_array_of_tables_mut)
    else {
        return false;
    };
    let tables: Vec<toml_edit::Table> = arr.iter().cloned().collect();
    let pos = |t: &toml_edit::Table| {
        let n = t.get("name").and_then(Item::as_str).unwrap_or("");
        names.iter().position(|x| *x == n).unwrap_or(usize::MAX)
    };
    let mut sorted = tables.clone();
    sorted.sort_by_key(|t| pos(t));
    let same = sorted
        .iter()
        .zip(&tables)
        .all(|(a, b)| a.get("name").and_then(Item::as_str) == b.get("name").and_then(Item::as_str));
    if same {
        return false;
    }
    arr.clear();
    for t in sorted {
        arr.push(t);
    }
    true
}

fn account_table<'a>(
    doc: &'a mut DocumentMut,
    name: &str,
    create: bool,
) -> Option<&'a mut toml_edit::Table> {
    if !doc.contains_key("account") {
        if !create {
            return None;
        }
        doc["account"] = Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let arr = doc.get_mut("account")?.as_array_of_tables_mut()?;
    let found = arr
        .iter()
        .position(|t| t.get("name").and_then(Item::as_str) == Some(name));
    let i = match found {
        Some(i) => i,
        None if create => {
            let mut t = toml_edit::Table::new();
            t["name"] = toml_edit::value(name);
            arr.push(t);
            arr.len() - 1
        }
        None => return None,
    };
    arr.get_mut(i)
}

/// Set one key; true when the value changed.
fn set_key(doc: &mut DocumentMut, key: &str, v: toml_edit::Value) -> anyhow::Result<bool> {
    let (t, last): (&mut dyn toml_edit::TableLike, String) = if let Some(r) =
        key.strip_prefix("account.")
    {
        let (name, field) = r
            .rsplit_once('.')
            .ok_or_else(|| anyhow::anyhow!("bad account key {key}"))?;
        if field == "name" {
            account_table(doc, name, true);
            return Ok(false);
        }
        let t = account_table(doc, name, true).ok_or_else(|| anyhow::anyhow!("account list"))?;
        (t, field.to_string())
    } else {
        let parts: Vec<&str> = key.split('.').collect();
        let (path, last) = parts.split_at(parts.len() - 1);
        let mut t: &mut dyn toml_edit::TableLike = doc.as_table_mut();
        for p in path {
            if t.get(p).is_none() {
                t.insert(p, toml_edit::table());
            }
            t = t
                .get_mut(p)
                .and_then(Item::as_table_like_mut)
                .ok_or_else(|| anyhow::anyhow!("{p} is not a table"))?;
        }
        (t, last[0].to_string())
    };
    let same = t
        .get(&last)
        .and_then(Item::as_value)
        .is_some_and(|old| old.to_string().trim() == v.to_string().trim());
    if same {
        return Ok(false);
    }
    let decor = t
        .get(&last)
        .and_then(Item::as_value)
        .map(|o| o.decor().clone());
    let mut v = v;
    if let Some(d) = decor {
        *v.decor_mut() = d;
    }
    // In place: the key (and the comment above it) stays.
    match t.get_mut(&last) {
        Some(item) => *item = Item::Value(v),
        None => {
            t.insert(&last, Item::Value(v));
        }
    }
    Ok(true)
}

fn remove_key(doc: &mut DocumentMut, key: &str) -> bool {
    if let Some(r) = key.strip_prefix("account.") {
        let Some((name, field)) = r.rsplit_once('.') else {
            return false;
        };
        return account_table(doc, name, false)
            .and_then(|t| t.remove(field))
            .is_some();
    }
    let parts: Vec<&str> = key.split('.').collect();
    let (path, last) = parts.split_at(parts.len() - 1);
    let mut t: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for p in path {
        match t.get_mut(p).and_then(Item::as_table_like_mut) {
            Some(x) => t = x,
            None => return false,
        }
    }
    t.remove(last[0]).is_some()
}

/// The portable keys as a readable TOML file (config/portable.toml).
pub fn render(keys: &BTreeMap<String, Value>) -> String {
    let mut s = String::from(
        "# GodTerm portable config: every key except machine local ones.\n\
         # Written by GodTerm Sync; edit config.toml on a machine instead.\n",
    );
    for (k, v) in keys {
        let tv = to_toml(v)
            .map(|x| x.to_string().trim().to_string())
            .unwrap_or_else(|| "\"\"".into());
        let qk: Vec<String> = k
            .split('.')
            .map(|p| {
                if p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    p.to_string()
                } else {
                    format!("{p:?}")
                }
            })
            .collect();
        s.push_str(&format!("{} = {tv}\n", qk.join(".")));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_style_config_paths_count_as_absolute() {
        for p in ["/x", "C:\\Users\\a", "d:/x", "\\\\srv\\share"] {
            assert!(is_abs_text(p), "{p}");
        }
        for p in ["~/x", "rel/x", "C:", "@n/x"] {
            assert!(!is_abs_text(p), "{p}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_map_to_portable_and_back() {
        let m = PathMap::new(Path::new("C:\\Users\\alice"), &BTreeMap::new());
        assert_eq!(
            m.to_portable(Path::new("C:\\Users\\alice\\code\\api")),
            "~/code/api"
        );
        assert_eq!(m.portable_text("C:\\Users\\alice\\code"), "~/code");
        assert_eq!(
            m.to_local("~/code/api"),
            Some(PathBuf::from("C:\\Users\\alice\\code\\api"))
        );
    }

    #[test]
    fn path_map_round_trips_between_homes() {
        let mut named = BTreeMap::new();
        named.insert("work".to_string(), "/Volumes/Work".to_string());
        let mac = PathMap::new(Path::new("/Users/alice"), &named);
        let mut named2 = BTreeMap::new();
        named2.insert("work".to_string(), "/mnt/work".to_string());
        let linux = PathMap::new(Path::new("/home/alice"), &named2);
        for (p, port) in [
            ("/Users/alice/notes-app", "~/notes-app"),
            ("/Users/alice", "~"),
            ("/Volumes/Work/api", "@work/api"),
            ("/opt/thing", "/opt/thing"),
        ] {
            assert_eq!(mac.to_portable(Path::new(p)), port);
            assert_eq!(mac.to_local(port).unwrap(), PathBuf::from(p));
        }
        assert_eq!(
            linux.to_local("~/notes-app").unwrap(),
            PathBuf::from("/home/alice/notes-app")
        );
        assert_eq!(
            linux.to_local("@work/api").unwrap(),
            PathBuf::from("/mnt/work/api")
        );
        assert_eq!(
            PathMap::new(Path::new("/h"), &BTreeMap::new()).to_local("@nope/x"),
            None
        );
        assert_eq!(
            mac.project_dir_name("~/notes-app").unwrap(),
            "-Users-alice-notes-app"
        );
        assert_eq!(
            linux.project_dir_name("~/notes-app").unwrap(),
            "-home-alice-notes-app"
        );
        assert_eq!(
            linux
                .project_dir_name(&mac.to_portable(Path::new("/Users/alice/a b/c.d")))
                .unwrap(),
            "-home-alice-a-b-c-d"
        );
        assert_eq!(path_key("~/notes-app").len(), "notes-app-".len() + 8);
        assert_ne!(path_key("~/a"), path_key("@a"));
    }

    #[test]
    fn config_split_keeps_local_keys_and_comments() {
        let map = PathMap::new(Path::new("/Users/alice"), &BTreeMap::new());
        let local = r#"# my config
claude_bin = "/opt/claude"   # local binary
remember_window = true
trusted_dirs = ["/Users/alice/code"]
permission_mode = "bypass"

[voice]
# speed comment
tts_speed = 1.0
device = "MacBook Mic"

[web]
enabled = true

[[account]]
name = "work"
label = "Work"
config_dir = "/Users/alice/.claude-work"
"#;
        let keys = flatten(local, &map, &[]).unwrap();
        assert!(keys.contains_key("voice.tts_speed"));
        assert!(keys.contains_key("account.work.label"));
        for k in [
            "claude_bin",
            "remember_window",
            "voice.device",
            "web.enabled",
            "account.work.config_dir",
        ] {
            assert!(!keys.contains_key(k), "{k} is local");
        }
        assert_eq!(keys["trusted_dirs"], serde_json::json!(["~/code"]));

        // Pulled from another machine: a new speed, a new account, a
        // trusted dir, and keys that must never apply.
        let mut pulled = BTreeMap::new();
        pulled.insert("voice.tts_speed".to_string(), serde_json::json!(1.25));
        pulled.insert("account.home.name".to_string(), serde_json::json!("home"));
        pulled.insert("account.home.label".to_string(), serde_json::json!("Home"));
        pulled.insert(
            "trusted_dirs".to_string(),
            serde_json::json!(["~/code", "~/new"]),
        );
        pulled.insert("claude_bin".to_string(), serde_json::json!("/tmp/evil"));
        pulled.insert("web.enabled".to_string(), serde_json::json!(true));
        pulled.insert("supervisor.enabled".to_string(), serde_json::json!(true));
        pulled.insert(
            "account.work.args".to_string(),
            serde_json::json!(["--x; rm -rf ~"]),
        );
        pulled.insert(
            "account.work.config_dir".to_string(),
            serde_json::json!("/tmp/x"),
        );
        let other = PathMap::new(Path::new("/home/alice"), &BTreeMap::new());
        let (out, ap) = apply(local, &pulled, &["permission_mode".to_string()], &other).unwrap();
        assert!(
            out.contains("# my config")
                && out.contains("# speed comment")
                && out.contains("# local binary"),
            "{out}"
        );
        assert!(out.contains("claude_bin = \"/opt/claude\""));
        assert!(out.contains("tts_speed = 1.25"));
        assert!(out.contains("device = \"MacBook Mic\""));
        assert!(out.contains("config_dir = \"/Users/alice/.claude-work\""));
        assert!(out.contains("name = \"home\"") && out.contains("label = \"Home\""));
        assert!(out.contains(r#"trusted_dirs = ["~/code", "~/new"]"#));
        assert!(!out.contains("evil") && !out.contains("rm -rf") && !out.contains("/tmp/x"));
        assert!(!out.contains("permission_mode"));
        assert_eq!(ap.refused.len(), 5, "{:?}", ap.refused);
        let cfg = crate::config::Config::parse(&out).unwrap();
        assert_eq!(cfg.accounts.len(), 2);
        // A second apply of the same values changes nothing.
        let (out2, ap2) = apply(&out, &pulled, &[], &other).unwrap();
        assert_eq!(out2, out);
        assert!(ap2.changed.is_empty());
        let r = render(&keys);
        assert!(r.contains("voice.tts_speed = 1.0") && r.contains("account.work.label = \"Work\""));
    }
}
