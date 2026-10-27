use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

pub const IGNORED: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "venv",
    "env",
    "__pycache__",
    "coverage",
    "bower_components",
    "Pods",
    "DerivedData",
    "site-packages",
    "tmp",
    "temp",
    "logs",
    "bin",
    "obj",
];

pub fn ignored_dir(name: &str) -> bool {
    name.starts_with('.') || IGNORED.contains(&name)
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub depth: usize,
    pub max_dirs: usize,
    pub max_manifests: usize,
    pub per_root: Duration,
    pub total: Duration,
    pub max_roots: usize,
    pub max_file: u64,

    pub recheck: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            depth: 3,
            max_dirs: 400,
            max_manifests: 80,
            per_root: Duration::from_millis(150),
            total: Duration::from_millis(1500),
            max_roots: 300,
            max_file: 64 * 1024,
            recheck: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub rel: String,

    pub manifest: Option<String>,

    pub name: Option<String>,

    pub desc: Option<String>,

    #[serde(default)]
    pub member: bool,

    #[serde(default)]
    pub git: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RootMap {
    pub root: PathBuf,
    pub entries: Vec<Entry>,

    pub workspaces: Vec<String>,

    pub remote: Option<String>,

    pub watched: Vec<(PathBuf, u128, u64)>,

    pub truncated: bool,
    #[serde(skip)]
    pub checked: Option<Instant>,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub scanned: usize,
    pub reused: usize,
    pub pending: usize,
    pub took_ms: u128,
}

#[derive(Default)]
pub struct Index {
    pub roots: HashMap<PathBuf, RootMap>,
    pub building: bool,
    pub last: Stats,
    loaded: bool,

    pub git: HashMap<PathBuf, crate::project_git::Facts>,

    pub git_checked: Option<Instant>,

    pub git_fetching: bool,
}

pub type Shared = Arc<Mutex<Index>>;

pub fn disk_path() -> PathBuf {
    crate::config::app_home().join("project-index.json")
}

const DISK_VERSION: u32 = 2;

#[derive(Serialize, Deserialize, Default)]
struct Disk {
    version: u32,
    roots: Vec<RootMap>,
    #[serde(default)]
    git: Vec<crate::project_git::Facts>,
}

pub fn load(shared: &Shared) {
    let mut ix = shared.lock().unwrap_or_else(|e| e.into_inner());
    if ix.loaded || ix.building {
        return;
    }
    load_into(&mut ix);
}

fn load_into(ix: &mut Index) {
    ix.loaded = true;
    if cfg!(test) {
        return;
    }
    if let Ok(t) = std::fs::read(disk_path()) {
        if let Ok(d) = serde_json::from_slice::<Disk>(&t) {
            if d.version == DISK_VERSION {
                for r in d.roots {
                    ix.roots.entry(r.root.clone()).or_insert(r);
                }
                for g in d.git {
                    ix.git.entry(g.top.clone()).or_insert(g);
                }
            }
        }
    }
}

pub fn save(shared: &Shared) {
    if cfg!(test) {
        return;
    }
    let d = {
        let ix = shared.lock().unwrap_or_else(|e| e.into_inner());
        Disk {
            version: DISK_VERSION,
            roots: ix.roots.values().cloned().collect(),
            git: ix.git.values().cloned().collect(),
        }
    };
    if let Ok(t) = serde_json::to_vec(&d) {
        let tmp = disk_path().with_extension("json.tmp");
        if crate::config::write_private(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, disk_path());
        }
    }
}

fn stamp(p: &Path) -> Option<(u128, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((t, m.len()))
}

fn read_small(p: &Path, max: u64) -> Option<String> {
    use std::io::Read;
    let name = p.file_name()?.to_string_lossy();
    if crate::guard::secret_name(&name) {
        return None;
    }
    let md = std::fs::metadata(p).ok()?;
    if !md.is_file() {
        return None;
    }
    let mut buf = vec![];
    std::fs::File::open(p)
        .ok()?
        .take(max)
        .read_to_end(&mut buf)
        .ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn clip(s: &str) -> String {
    crate::sessions::snippet(s.trim(), 160)
}

fn readme_line(text: &str) -> Option<String> {
    let mut first = None;
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(h) = l.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim();
            if !h.is_empty() {
                return Some(clip(h));
            }
        } else if first.is_none()
            && !l.starts_with('<')
            && !l.starts_with('[')
            && !l.starts_with('!')
            && !l.starts_with("===")
        {
            first = Some(clip(l));
        }
    }
    first
}

type Manifest = (Option<String>, Option<String>, Vec<String>, Option<String>);

fn parse_manifest(file: &str, text: &str) -> Manifest {
    let s = |v: Option<&str>| v.filter(|x| !x.trim().is_empty()).map(clip);
    match file {
        "package.json" => {
            let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
            let ws = match &v["workspaces"] {
                serde_json::Value::Array(a) => a.clone(),
                o => o["packages"].as_array().cloned().unwrap_or_default(),
            };
            let ws: Vec<String> = ws
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect();
            let tool = (!ws.is_empty()).then(|| "npm workspaces".to_string());
            (
                s(v["name"].as_str()),
                s(v["description"].as_str()),
                ws,
                tool,
            )
        }
        "Cargo.toml" | "pyproject.toml" => {
            let v: toml::Value = toml::from_str(text).unwrap_or(toml::Value::Integer(0));
            let get = |t: &str, k: &str| {
                v.get(t)
                    .and_then(|x| x.get(k))
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            };
            let poetry = |k: &str| {
                v.get("tool")
                    .and_then(|x| x.get("poetry"))
                    .and_then(|x| x.get(k))
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            };
            let (table, ws_key) = if file == "Cargo.toml" {
                ("package", "workspace")
            } else {
                ("project", "")
            };
            let ws: Vec<String> = v
                .get(ws_key)
                .and_then(|w| w.get("members"))
                .and_then(|m| m.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let tool = v
                .get(ws_key)
                .is_some()
                .then(|| "cargo workspace".to_string());
            (
                s(get(table, "name").or_else(|| poetry("name")).as_deref()),
                s(get(table, "description")
                    .or_else(|| poetry("description"))
                    .as_deref()),
                ws,
                tool,
            )
        }
        "go.mod" => {
            let m = text
                .lines()
                .find_map(|l| l.trim().strip_prefix("module "))
                .map(|m| m.trim().to_string());
            (s(m.as_deref()), None, vec![], None)
        }
        "pom.xml" => {
            let tag = |t: &str| {
                let open = format!("<{t}>");
                let body = text.split("<dependencies>").next().unwrap_or(text);

                let body = match body.find("</parent>") {
                    Some(i) => &body[i..],
                    None => body,
                };
                let i = body.find(&open)? + open.len();
                let j = body[i..].find('<')? + i;
                Some(body[i..j].trim().to_string())
            };
            (
                s(tag("name").or_else(|| tag("artifactId")).as_deref()),
                s(tag("description").as_deref()),
                vec![],
                None,
            )
        }
        "pnpm-workspace.yaml" | "lerna.json" => {
            let ws: Vec<String> = if file == "lerna.json" {
                let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
                v["packages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            } else {
                text.lines()
                    .filter_map(|l| l.trim().strip_prefix("- "))
                    .map(|l| l.trim().trim_matches(['\'', '"']).to_string())
                    .filter(|l| !l.starts_with('!'))
                    .collect()
            };
            let tool = if file == "lerna.json" {
                "lerna"
            } else {
                "pnpm workspaces"
            };
            (None, None, ws, Some(tool.into()))
        }
        f if f.ends_with(".csproj") => {
            let stem = f.trim_end_matches(".csproj").to_string();
            let desc = text.find("<Description>").and_then(|i| {
                let b = &text[i + 13..];
                b.find('<').map(|j| b[..j].to_string())
            });
            (s(Some(&stem)), s(desc.as_deref()), vec![], None)
        }
        _ => (None, None, vec![], None),
    }
}

const MANIFESTS: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
];

fn is_manifest(name: &str) -> bool {
    MANIFESTS.contains(&name) || name.ends_with(".csproj")
}

fn is_readme(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "readme.md" || n == "readme" || n == "readme.txt" || n == "readme.rst"
}

fn git_remote(root: &Path) -> Option<String> {
    let text = read_small(&root.join(".git").join("config"), 32 * 1024)?;
    let mut in_origin = false;
    let mut any = None;
    for l in text.lines().map(str::trim) {
        if l.starts_with('[') {
            in_origin = l == "[remote \"origin\"]";
            continue;
        }
        if let Some(u) = l
            .strip_prefix("url")
            .map(|r| r.trim_start_matches([' ', '=']))
        {
            let name = u
                .trim()
                .trim_end_matches('/')
                .rsplit(['/', ':'])
                .next()
                .unwrap_or("")
                .trim_end_matches(".git")
                .to_string();
            if name.is_empty() {
                continue;
            }
            if in_origin {
                return Some(name);
            }
            any.get_or_insert(name);
        }
    }
    any
}

fn expand_members(root: &Path, globs: &[String], cap: usize) -> Vec<PathBuf> {
    let mut out = vec![];
    for g in globs {
        let g = g.trim_start_matches("./").trim_end_matches('/');
        if g.contains("..") || g.starts_with('/') {
            continue;
        }
        let (base, wild) = match g.split_once('*') {
            Some((b, _)) => (b.trim_end_matches('/'), true),
            None => (g, false),
        };
        let dir = root.join(base);
        if !wild {
            if dir.is_dir() {
                out.push(dir);
            }
        } else if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut v: Vec<PathBuf> = rd
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter(|e| !ignored_dir(&e.file_name().to_string_lossy()))
                .map(|e| e.path())
                .collect();
            v.sort();
            out.extend(v);
        }
        if out.len() >= cap {
            break;
        }
    }
    out.truncate(cap);
    out
}

fn rel_of(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

fn read_dir_info(
    dir: &Path,
    names: &[String],
    lim: &Limits,
    watched: &mut Vec<(PathBuf, u128, u64)>,
) -> (Entry, Vec<String>, Vec<String>) {
    let mut e = Entry::default();
    let (mut members, mut tools) = (vec![], vec![]);
    let mut readme = None;
    let mut ordered: Vec<&String> = names.iter().collect();

    ordered.sort_by_key(|n| {
        MANIFESTS
            .iter()
            .position(|m| m == n)
            .unwrap_or(MANIFESTS.len())
    });
    for n in ordered {
        let ws_file = n == "pnpm-workspace.yaml" || n == "lerna.json";
        let tool_file = n == "turbo.json" || n == "nx.json";
        if !(is_manifest(n) || is_readme(n) || ws_file || tool_file) {
            continue;
        }
        let p = dir.join(n);
        if crate::guard::denied(&p, &[]).is_some() {
            continue;
        }
        if let Some((m, l)) = stamp(&p) {
            watched.push((p.clone(), m, l));
        }
        if tool_file {
            tools.push(n.trim_end_matches(".json").to_string());
            continue;
        }
        let Some(text) = read_small(&p, lim.max_file) else {
            continue;
        };
        if is_readme(n) {
            readme = readme_line(&text);
            continue;
        }
        let (name, desc, ws, tool) = parse_manifest(n, &text);
        members.extend(ws);
        tools.extend(tool);
        if is_manifest(n) && e.manifest.is_none() {
            e.manifest = Some(n.clone());
            e.name = name;
            e.desc = desc;
        }
    }
    if e.desc.is_none() {
        e.desc = readme;
    }
    (e, members, tools)
}

pub fn scan_root(root: &Path, lim: &Limits, allowed: &[PathBuf]) -> RootMap {
    let t0 = Instant::now();
    let mut map = RootMap {
        root: root.to_path_buf(),
        ..Default::default()
    };
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut queue: VecDeque<(PathBuf, usize, bool)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0, false));
    let mut manifests = 0;
    let mut root_members: Vec<String> = vec![];
    let mut members_done = false;
    while let Some((dir, depth, member)) = queue.pop_front() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        if seen.len() > lim.max_dirs || t0.elapsed() > lim.per_root {
            map.truncated = true;
            break;
        }
        if crate::guard::denied(&dir, allowed).is_some() {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        if let Some((m, l)) = stamp(&dir) {
            map.watched.push((dir.clone(), m, l));
        }
        let (mut files, mut dirs) = (vec![], vec![]);
        let mut is_git = false;
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n == ".git" {
                is_git = true;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() && !ignored_dir(&n) => dirs.push(n),
                Ok(t) if t.is_file() => files.push(n),
                _ => {}
            }
        }
        dirs.sort();
        let (mut entry, members, tools) = if manifests < lim.max_manifests {
            read_dir_info(&dir, &files, lim, &mut map.watched)
        } else {
            map.truncated = true;
            (Entry::default(), vec![], vec![])
        };
        if entry.manifest.is_some() {
            manifests += 1;
        }
        entry.rel = rel_of(root, &dir);
        entry.member = member;
        entry.git = is_git;
        if depth == 0 {
            root_members = members;
            map.workspaces = tools;
            map.workspaces.dedup();
        }
        map.entries.push(entry);
        if depth < lim.depth {
            for d in dirs {
                queue.push_back((dir.join(d), depth + 1, false));
            }
        }

        if depth == 0 && !members_done {
            members_done = true;
            for m in expand_members(root, &root_members, lim.max_dirs / 2)
                .into_iter()
                .rev()
            {
                queue.push_front((m, 1, true));
            }
        }
    }

    let member_dirs: HashSet<String> = expand_members(root, &root_members, lim.max_dirs / 2)
        .iter()
        .map(|p| rel_of(root, p))
        .collect();
    for e in map.entries.iter_mut() {
        if member_dirs.contains(&e.rel) {
            e.member = true;
        }
    }
    map.remote = git_remote(root);
    map.checked = Some(Instant::now());
    map
}

pub fn unchanged(map: &RootMap) -> bool {
    map.watched
        .iter()
        .all(|(p, m, l)| stamp(p) == Some((*m, *l)))
}

pub fn good_root(p: &Path, allowed: &[PathBuf]) -> bool {
    let home = crate::config::home_dir();
    p.is_absolute()
        && p.is_dir()
        && p != Path::new("/")
        && !home.starts_with(p)
        && crate::guard::denied(p, allowed).is_none()
}

pub fn repo_of(p: &Path) -> Option<PathBuf> {
    let home = crate::config::home_dir();
    let mut cur = p.parent();
    for _ in 0..4 {
        let c = cur?;
        if home.starts_with(c) {
            return None;
        }
        if c.join(".git").exists() {
            return Some(c.to_path_buf());
        }
        cur = c.parent();
    }
    None
}

pub fn refresh(shared: &Shared, roots: &[PathBuf], lim: &Limits, force: bool, allowed: &[PathBuf]) {
    let t0 = Instant::now();
    let (mut have, loaded) = {
        let mut ix = shared.lock().unwrap_or_else(|e| e.into_inner());
        let loaded = ix.loaded;
        if !loaded {
            load_into(&mut ix);
        }
        ix.building = true;
        (std::mem::take(&mut ix.roots), loaded)
    };
    let mut want: Vec<PathBuf> = vec![];
    let mut seen = HashSet::new();
    for r in roots {
        if want.len() >= lim.max_roots {
            break;
        }
        if seen.insert(r.clone()) && good_root(r, allowed) {
            want.push(r.clone());
        }
    }
    let mut out: HashMap<PathBuf, RootMap> = HashMap::new();
    let mut st = Stats::default();
    for r in &want {
        let old = have.remove(r);
        let due = old
            .as_ref()
            .is_none_or(|m| force || m.checked.is_none_or(|c| c.elapsed() >= lim.recheck));
        let keep = match old {
            Some(m) if !due => {
                st.reused += 1;
                Some(m)
            }
            Some(mut m) if unchanged(&m) => {
                m.checked = Some(Instant::now());
                st.reused += 1;
                Some(m)
            }
            other => {
                if t0.elapsed() < lim.total {
                    st.scanned += 1;
                    Some(scan_root(r, lim, allowed))
                } else {
                    st.pending += 1;
                    other
                }
            }
        };
        if let Some(m) = keep {
            out.insert(r.clone(), m);
        }
    }
    let changed = st.scanned > 0 || !have.is_empty() || !loaded;
    st.took_ms = t0.elapsed().as_millis();
    let mut ix = shared.lock().unwrap_or_else(|e| e.into_inner());
    if st.scanned > 0 {
        crate::log::info(&format!(
            "project index: {} roots ({} scanned, {} unchanged, {} waiting) in {} ms",
            out.len(),
            st.scanned,
            st.reused,
            st.pending,
            st.took_ms
        ));
    }
    ix.roots = out;
    ix.loaded = true;
    ix.building = false;
    ix.last = st;
    drop(ix);
    if changed {
        save(shared);
    }
}

pub fn refresh_from_sessions(
    shared: &Shared,
    sessions: &crate::session_index::Shared,
    extra: &[PathBuf],
    force: bool,
    allowed: &[PathBuf],
) {
    let mut roots: Vec<PathBuf> = vec![];
    {
        let ix = sessions.lock().unwrap_or_else(|e| e.into_inner());
        let mut seen = HashSet::new();
        for r in ix
            .rows
            .iter()
            .filter(|r| r.info.parent.is_none() && !crate::sess_sort::headless(&r.info))
        {
            if seen.insert(r.info.cwd.clone()) {
                roots.push(PathBuf::from(&r.info.cwd));
            }
        }
    }
    roots.extend(extra.iter().cloned());
    let mut all = vec![];
    for r in roots {
        if let Some(repo) = repo_of(&r) {
            all.push(repo);
        }
        all.push(r);
    }
    refresh(shared, &all, &Limits::default(), force, allowed);
}

const STOP: &[&str] = &[
    "the",
    "a",
    "an",
    "my",
    "our",
    "that",
    "this",
    "thing",
    "things",
    "stuff",
    "project",
    "projects",
    "repo",
    "one",
    "of",
    "in",
    "for",
    "with",
    "on",
    "to",
    "and",
    "where",
    "was",
    "is",
    "it",
    "we",
    "i",
    "folder",
    "directory",
    "dir",
    "code",
    "called",
    "named",
    "about",
    "some",
    "find",
    "open",
    "me",
    "had",
    "have",
    "working",
    "worked",
    "were",
];

pub fn tokens(s: &str) -> Vec<String> {
    let s = crate::fuzzy::spoken_punctuation(s);
    let mut out = vec![];
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if c.is_uppercase() && prev_lower && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
            cur.extend(c.to_lowercase());
        } else {
            prev_lower = false;
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn query_tokens(q: &str) -> Vec<String> {
    let all = tokens(q);
    let kept: Vec<String> = all
        .iter()
        .filter(|w| !STOP.contains(&w.as_str()))
        .cloned()
        .collect();
    if kept.is_empty() {
        all
    } else {
        kept
    }
}

fn singular(w: &str) -> std::borrow::Cow<'_, str> {
    if let Some(s) = w.strip_suffix("ies").filter(|s| s.len() >= 3) {
        return format!("{s}y").into();
    }
    w.strip_suffix("es")
        .filter(|s| s.len() >= 3 && (s.ends_with('s') || s.ends_with('x') || s.ends_with("sh")))
        .or_else(|| w.strip_suffix('s').filter(|s| s.len() >= 3))
        .unwrap_or(w)
        .into()
}

pub fn word_match(q: &str, c: &str) -> f64 {
    if q == c {
        return 1.0;
    }
    if singular(q) == singular(c) {
        return 0.97;
    }
    let (s, l) = if q.len() <= c.len() { (q, c) } else { (c, q) };
    if s.len() >= 4 && l.starts_with(s) && l.len() - s.len() <= 3 {
        return 0.9;
    }
    if s.len() >= 4 {
        let sim = crate::fuzzy::similarity(q, c);
        if sim >= 0.85 {
            return sim * 0.95;
        }
    }
    0.0
}

fn with_pairs(words: &[String]) -> Vec<String> {
    let mut v = words.to_vec();
    for w in words.windows(2) {
        v.push(format!("{}{}", w[0], w[1]));
    }
    v
}

pub struct Field<'a> {
    pub text: &'a str,
    pub tier: f64,

    pub context: bool,
    pub why: &'static str,
}

pub fn score_fields(q: &[String], fields: &[Field]) -> Option<(f64, &'static str, f64)> {
    if q.is_empty() {
        return None;
    }
    let toks: Vec<Vec<String>> = fields.iter().map(|f| with_pairs(&tokens(f.text))).collect();
    let mut best: Vec<(f64, usize)> = vec![(0.0, usize::MAX); q.len()];
    let mut give = |i: usize, v: f64, f: usize| {
        if v > best[i].0 {
            best[i] = (v, f);
        }
    };
    for (fi, f) in fields.iter().enumerate() {
        for (i, w) in q.iter().enumerate() {
            let m = toks[fi]
                .iter()
                .map(|c| word_match(w, c))
                .fold(0.0, f64::max);
            give(i, m * f.tier, fi);
        }

        for i in 0..q.len().saturating_sub(1) {
            let joined = format!("{}{}", q[i], q[i + 1]);
            let m = toks[fi]
                .iter()
                .map(|c| word_match(&joined, c))
                .fold(0.0, f64::max);
            give(i, m * f.tier, fi);
            give(i + 1, m * f.tier, fi);
        }
    }
    let matched: Vec<&(f64, usize)> = best.iter().filter(|(v, _)| *v > 0.0).collect();
    if matched.is_empty() || !matched.iter().any(|(_, f)| !fields[*f].context) {
        return None;
    }
    let cov = matched.len() as f64 / q.len() as f64;
    let mean = matched.iter().map(|(v, _)| v).sum::<f64>() / matched.len() as f64;

    let mut by: HashMap<usize, f64> = HashMap::new();
    for (v, f) in &matched {
        if !fields[*f].context {
            *by.entry(*f).or_default() += v;
        }
    }
    let lead = by
        .into_iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(f, _)| fields[f].why)
        .unwrap_or("name");
    Some((mean * (0.55 + 0.45 * cov), lead, cov))
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub path: PathBuf,
    pub root: PathBuf,
    pub rel: String,
    pub score: f64,
    pub why: String,
    pub matched_on: &'static str,
}

fn quote(s: &str) -> String {
    format!("\"{}\"", crate::sessions::snippet(s, 80))
}

pub fn search(ix: &Index, query: &str, limit: usize) -> Vec<Hit> {
    let q = query_tokens(query);
    let mut hits: HashMap<PathBuf, Hit> = HashMap::new();
    for m in ix.roots.values() {
        let root_name = m
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let root_t = crate::config::tilde(&m.root);
        for e in &m.entries {
            let base = e.rel.rsplit('/').next().unwrap_or("");
            let sub = e.manifest.is_some() || e.member;
            let mut fields: Vec<Field> = vec![];
            let parents = e.rel.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            if e.rel.is_empty() {
                fields.push(Field {
                    text: &root_name,
                    tier: 1.0,
                    context: false,
                    why: "project",
                });
                if let Some(n) = &e.name {
                    fields.push(Field {
                        text: n,
                        tier: 1.0,
                        context: false,
                        why: "project",
                    });
                }
                if let Some(r) = &m.remote {
                    fields.push(Field {
                        text: r,
                        tier: 0.95,
                        context: false,
                        why: "remote",
                    });
                }
            } else {
                let tier = if sub { 0.92 } else { 0.85 };
                let why = if sub { "subproject" } else { "folder" };
                fields.push(Field {
                    text: base,
                    tier,
                    context: false,
                    why,
                });
                if let Some(n) = &e.name {
                    fields.push(Field {
                        text: n,
                        tier: 0.92,
                        context: false,
                        why: "subproject",
                    });
                }
                fields.push(Field {
                    text: parents,
                    tier: 0.6,
                    context: true,
                    why: "folder",
                });
                fields.push(Field {
                    text: &root_name,
                    tier: 0.6,
                    context: true,
                    why: "folder",
                });
            }
            if let Some(d) = &e.desc {
                fields.push(Field {
                    text: d,
                    tier: 0.75,
                    context: false,
                    why: "description",
                });
            }
            let Some((sc, on, cov)) = score_fields(&q, &fields) else {
                continue;
            };
            if cov < 0.5 || sc < 0.5 {
                continue;
            }
            let path = if e.rel.is_empty() {
                m.root.clone()
            } else {
                m.root.join(&e.rel)
            };
            let what = if e.member {
                "workspace member"
            } else if e.manifest.is_some() {
                "subproject"
            } else {
                "folder"
            };
            let why = match (e.rel.is_empty(), on) {
                (true, "remote") => format!(
                    "project {root_t} (git remote {})",
                    m.remote.as_deref().unwrap_or("")
                ),
                (true, "description") => format!(
                    "project {root_t}: {}",
                    quote(e.desc.as_deref().unwrap_or(""))
                ),
                (true, _) => format!("project {root_t}"),
                (false, "description") => format!(
                    "{what} {} in {root_t}: {}",
                    e.rel,
                    quote(e.desc.as_deref().unwrap_or(""))
                ),
                (false, _) => {
                    let mut s = format!("{what} {} in {root_t}", e.rel);
                    if let (Some(n), Some(f)) = (&e.name, &e.manifest) {
                        if crate::fuzzy::key(n) != crate::fuzzy::key(base) {
                            s.push_str(&format!(" ({f} name {n})"));
                        }
                    }
                    s
                }
            };
            let h = Hit {
                path: path.clone(),
                root: m.root.clone(),
                rel: e.rel.clone(),
                score: sc,
                why,
                matched_on: on,
            };
            match hits.get(&path) {
                Some(o) if o.score >= sc => {}
                _ => {
                    hits.insert(path, h);
                }
            }
        }
    }
    let mut v: Vec<Hit> = hits.into_values().collect();
    v.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.rel.len().cmp(&b.rel.len()))
    });
    v.truncate(limit);
    v
}

pub fn touched_score(q: &[String], cwd: &str, files: &[String]) -> Option<(f64, String)> {
    let mut best: Option<(f64, String)> = None;
    for f in files {
        let rel = f
            .strip_prefix(cwd)
            .map(|r| r.trim_start_matches('/'))
            .unwrap_or(f);
        let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        let stem = name.split('.').next().unwrap_or(name);
        let fields = [
            Field {
                text: stem,
                tier: 0.6,
                context: false,
                why: "touched",
            },
            Field {
                text: dir,
                tier: 0.6,
                context: false,
                why: "touched",
            },
        ];
        if let Some((sc, _, cov)) = score_fields(q, &fields) {
            if cov >= 0.5 && best.as_ref().is_none_or(|(b, _)| sc > *b) {
                best = Some((sc, rel.to_string()));
            }
        }
    }
    best
}

#[derive(Debug, Clone, Serialize)]
pub struct DeepHit {
    pub path: PathBuf,
    pub kind: &'static str,
    pub score: f64,
    pub why: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DeepResult {
    pub hits: Vec<DeepHit>,
    pub roots: usize,
    pub seen: usize,
    pub files_read: usize,
    pub truncated: bool,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct DeepLimits {
    pub depth: usize,
    pub max_entries: usize,
    pub names_time: Duration,
    pub content_time: Duration,
    pub max_file: u64,
    pub max_hits: usize,
}

impl Default for DeepLimits {
    fn default() -> Self {
        DeepLimits {
            depth: 6,
            max_entries: 20_000,
            names_time: Duration::from_millis(1500),
            content_time: Duration::from_millis(2000),
            max_file: 256 * 1024,
            max_hits: 30,
        }
    }
}

const TEXT_EXT: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "swift", "rb", "php",
    "cs", "c", "h", "cc", "cpp", "hpp", "m", "scala", "md", "txt", "json", "toml", "yaml", "yml",
    "html", "css", "scss", "vue", "svelte", "sql", "sh", "graphql", "proto", "xml", "ex", "exs",
    "dart", "lua",
];

pub fn deep_search(
    roots: &[PathBuf],
    query: &str,
    content: bool,
    lim: &DeepLimits,
    allowed: &[PathBuf],
) -> DeepResult {
    let q = query_tokens(query);
    let mut res = DeepResult {
        roots: roots.len(),
        ..Default::default()
    };
    let t0 = Instant::now();
    let mut files: Vec<PathBuf> = vec![];
    let mut name_hits: Vec<DeepHit> = vec![];
    let mut queue: VecDeque<(PathBuf, usize)> = roots.iter().map(|r| (r.clone(), 0)).collect();
    let mut seen_dirs = HashSet::new();
    'walk: while let Some((dir, depth)) = queue.pop_front() {
        if !seen_dirs.insert(dir.clone()) || crate::guard::denied(&dir, allowed).is_some() {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut ents: Vec<_> = rd.flatten().collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            if res.seen >= lim.max_entries || t0.elapsed() > lim.names_time {
                res.truncated = true;
                res.timed_out = t0.elapsed() > lim.names_time;
                break 'walk;
            }
            res.seen += 1;
            let n = e.file_name().to_string_lossy().into_owned();
            let Ok(t) = e.file_type() else { continue };
            let p = e.path();
            if n.starts_with('.') || crate::guard::secret_name(&n) {
                continue;
            }
            if t.is_dir() {
                if ignored_dir(&n) {
                    continue;
                }
                if depth < lim.depth {
                    queue.push_back((p.clone(), depth + 1));
                }
            } else if !t.is_file() {
                continue;
            }
            let stem = if t.is_dir() {
                n.as_str()
            } else {
                n.split('.').next().unwrap_or(&n)
            };
            let parent = dir.to_string_lossy();
            let fields = [
                Field {
                    text: stem,
                    tier: if t.is_dir() { 0.9 } else { 0.85 },
                    context: false,
                    why: "name",
                },
                Field {
                    text: &parent,
                    tier: 0.6,
                    context: true,
                    why: "name",
                },
            ];
            if let Some((sc, _, cov)) = score_fields(&q, &fields) {
                if cov >= 0.5 && sc >= 0.5 {
                    let kind = if t.is_dir() { "folder" } else { "file" };
                    name_hits.push(DeepHit {
                        why: format!("{kind} name {}", crate::config::tilde(&p)),
                        path: p.clone(),
                        kind,
                        score: sc,
                        line: None,
                    });
                }
            }
            if content && t.is_file() {
                let ext = n.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                if TEXT_EXT.contains(&ext.as_str()) {
                    files.push(p);
                }
            }
        }
    }
    let mut content_hits = vec![];
    if content && !q.is_empty() {
        let t1 = Instant::now();

        let words: Vec<String> = q
            .iter()
            .map(|w| {
                let w = singular(w);
                let n = w.chars().count();
                if n > 5 && w.ends_with('y') {
                    w.chars().take(n - 1).collect()
                } else {
                    w.to_string()
                }
            })
            .collect();
        for p in files {
            if t1.elapsed() > lim.content_time {
                res.timed_out = true;
                res.truncated = true;
                break;
            }
            if content_hits.len() >= lim.max_hits {
                res.truncated = true;
                break;
            }
            if std::fs::metadata(&p).map(|m| m.len()).unwrap_or(u64::MAX) > lim.max_file {
                continue;
            }
            let Some(text) = read_small(&p, lim.max_file) else {
                continue;
            };
            res.files_read += 1;
            if text.as_bytes().iter().take(1024).any(|b| *b == 0) {
                continue;
            }
            for (i, l) in text.lines().enumerate() {
                let low = l.to_lowercase();
                let key = crate::fuzzy::key(&low);
                if words
                    .iter()
                    .all(|w| low.contains(w.as_str()) || key.contains(w.as_str()))
                {
                    content_hits.push(DeepHit {
                        why: format!("line {} of {}", i + 1, crate::config::tilde(&p)),
                        path: p.clone(),
                        kind: "content",
                        score: 0.55,
                        line: Some(crate::sessions::snippet(l.trim(), 140)),
                    });
                    break;
                }
            }
        }
    }
    name_hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.path.as_os_str().len().cmp(&b.path.as_os_str().len()))
    });
    name_hits.truncate(lim.max_hits);
    res.hits = name_hits;
    res.hits.extend(content_hits);
    res.hits.truncate(lim.max_hits);
    res
}

#[cfg(test)]
#[path = "project_index_tests.rs"]
mod tests;
