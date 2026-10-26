//! Transcripts as append-only chunks (docs/SYNC.md 2.5). After a sync the
//! bytes since the last push become immutable chunks of at most
//! `chunk_mb`, cut at line ends where a line end is close enough;
//! concatenating a file's chunks gives the file byte for byte. A manifest
//! per session lists them. Local and remote are compared by prefix:
//! equal, local ahead (push the new bytes), remote ahead (extend the
//! local file), or diverged (never merged: the local file becomes a new
//! forked session and the remote version takes the original id).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub repo: String,
    pub path: String,
    pub off: u64,
    pub len: u64,
    pub sha256: String,
}

/// One file of a session (Claude's transcript, or one file of a Grok
/// session folder).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileMan {
    pub name: String,
    pub len: u64,
    pub sha256: String,
    pub chunks: Vec<Chunk>,
}

/// `sessions/<acct>/<id>.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub provider: String,
    pub account: String,
    pub session: String,
    /// Portable.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub title: String,
    /// The main transcript (Claude's `<id>.jsonl`).
    #[serde(default)]
    pub len: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub chunks: Vec<Chunk>,
    /// Grok: every file of the session folder, by its relative path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileMan>,
    #[serde(default)]
    pub forked_from: Option<String>,
    #[serde(default)]
    pub updated: String,
    /// The machine that wrote it last.
    #[serde(default)]
    pub machine: String,
}

impl Manifest {
    /// The main file's entry (Claude) as a FileMan.
    pub fn main(&self) -> FileMan {
        FileMan {
            name: format!("{}.jsonl", self.session),
            len: self.len,
            sha256: self.sha256.clone(),
            chunks: self.chunks.clone(),
        }
    }

    pub fn set_main(&mut self, f: FileMan) {
        self.len = f.len;
        self.sha256 = f.sha256;
        self.chunks = f.chunks;
    }

    /// Every file, as FileMans (Claude: the main one).
    pub fn all_files(&self) -> Vec<FileMan> {
        if self.provider == "claude" {
            vec![self.main()]
        } else {
            self.files.clone()
        }
    }
}

/// Where the next piece of `data` ends: at most `max` bytes, at the last
/// line end inside them when there is one (a longer line is cut at `max`).
pub fn split_points(data: &[u8], max: usize) -> Vec<usize> {
    let mut v = vec![];
    let mut start = 0;
    while start < data.len() {
        let end = (start + max).min(data.len());
        let cut = if end == data.len() {
            end
        } else {
            match data[start..end].iter().rposition(|&b| b == b'\n') {
                Some(i) => start + i + 1,
                None => end,
            }
        };
        v.push(cut);
        start = cut;
    }
    v
}

/// The chunk path of bytes at `off` of file `name` in session `id`.
pub fn chunk_path(account: &str, id: &str, name: &str, off: u64) -> String {
    if name == format!("{id}.jsonl") {
        format!("{account}/{id}/{off:012}.jsonl")
    } else {
        format!("{account}/{id}/{name}/{off:012}")
    }
}

/// SHA-256 of ranges of a file: `cuts` are increasing end offsets of
/// consecutive ranges from 0. Returns each range's hash and the hash of
/// 0..`whole_end`.
pub fn scan(p: &Path, cuts: &[u64], whole_end: u64) -> Result<(Vec<String>, String)> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(p).with_context(|| format!("opening {}", p.display()))?;
    let mut whole = Sha256::new();
    let mut part = Sha256::new();
    let mut parts = vec![];
    let mut pos = 0u64;
    let mut ci = 0;
    let end = cuts.last().copied().unwrap_or(0).max(whole_end);
    let mut buf = vec![0u8; 1 << 20];
    while pos < end {
        let want = ((end - pos) as usize).min(buf.len());
        let n = f.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        let mut i = 0;
        while i < n {
            let lim = if ci < cuts.len() { cuts[ci] } else { u64::MAX };
            let take = ((lim - pos) as usize).min(n - i);
            let slice = &buf[i..i + take];
            if pos < whole_end {
                let w = ((whole_end - pos) as usize).min(slice.len());
                whole.update(&slice[..w]);
            }
            if ci < cuts.len() {
                part.update(slice);
            }
            pos += take as u64;
            i += take;
            if ci < cuts.len() && pos == cuts[ci] {
                parts.push(super::hex(&part.finalize_reset()));
                ci += 1;
            }
        }
    }
    Ok((parts, super::hex(&whole.finalize())))
}

pub fn read_range(p: &Path, off: u64, len: u64) -> Result<Vec<u8>> {
    let mut f = std::fs::File::open(p)?;
    f.seek(SeekFrom::Start(off))?;
    let mut v = vec![0u8; len as usize];
    f.read_exact(&mut v)?;
    Ok(v)
}

/// How the local file stands against the remote one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cmp {
    Equal,
    LocalAhead,
    RemoteAhead,
    Diverged,
}

/// Compare a local file (`len` bytes) with a remote FileMan by prefix.
/// Whole remote chunks are compared by hash; only a chunk straddling the
/// shorter length is read (`read`).
pub fn compare(
    p: &Path,
    len: u64,
    remote: &FileMan,
    read: &dyn Fn(&Chunk) -> Result<Vec<u8>>,
) -> Result<Cmp> {
    let n = len.min(remote.len);
    let full: Vec<&Chunk> = remote
        .chunks
        .iter()
        .filter(|c| c.off + c.len <= n)
        .collect();
    let cuts: Vec<u64> = full.iter().map(|c| c.off + c.len).collect();
    // Chunks must tile the file from 0.
    let mut at = 0;
    for c in &remote.chunks {
        if c.off != at {
            bail!("remote chunks of {} do not tile the file", remote.name);
        }
        at += c.len;
    }
    if at != remote.len {
        bail!(
            "remote chunks of {} add up to {at}, not {}",
            remote.name,
            remote.len
        );
    }
    let (parts, _) = scan(p, &cuts, 0)?;
    let mut same =
        parts.len() == full.len() && parts.iter().zip(&full).all(|(h, c)| *h == c.sha256);
    let covered = cuts.last().copied().unwrap_or(0);
    if same && covered < n {
        let c = remote
            .chunks
            .iter()
            .find(|c| c.off == covered)
            .context("missing straddling chunk")?;
        let bytes = read(c)?;
        if super::sha_hex(&bytes) != c.sha256 {
            bail!("chunk {} does not match its hash", c.path);
        }
        let mine = read_range(p, covered, n - covered)?;
        same = bytes[..(n - covered) as usize] == mine[..];
    }
    Ok(match (same, len.cmp(&remote.len)) {
        (false, _) => Cmp::Diverged,
        (true, std::cmp::Ordering::Equal) => Cmp::Equal,
        (true, std::cmp::Ordering::Greater) => Cmp::LocalAhead,
        (true, std::cmp::Ordering::Less) => Cmp::RemoteAhead,
    })
}

/// The remote bytes from `from` to the end, from its chunks.
pub fn remote_bytes(
    remote: &FileMan,
    from: u64,
    read: &dyn Fn(&Chunk) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    let mut out = vec![];
    for c in &remote.chunks {
        if c.off + c.len <= from {
            continue;
        }
        let b = read(c)?;
        if super::sha_hex(&b) != c.sha256 || b.len() as u64 != c.len {
            bail!("chunk {} does not match its hash", c.path);
        }
        let skip = from.saturating_sub(c.off) as usize;
        out.extend_from_slice(&b[skip..]);
    }
    Ok(out)
}

/// Append remote bytes to a local file that is still `len` long.
pub fn extend(p: &Path, len: u64, bytes: &[u8]) -> Result<()> {
    let md = std::fs::symlink_metadata(p)?;
    if md.file_type().is_symlink() || md.len() != len {
        bail!("{} changed while extending", p.display());
    }
    let mut f = std::fs::OpenOptions::new().append(true).open(p)?;
    f.write_all(bytes)?;
    f.flush()?;
    Ok(())
}

/// Write a whole file from remote bytes (temp file and rename).
pub fn place(p: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("{} is a symlink: not replaced", p.display());
    }
    super::write_file(p, bytes)?;
    Ok(())
}

/// A transcript's title: its last `/rename`, else its first prompt.
pub fn claude_title(head: &[u8], tail_titles: Option<String>) -> String {
    if let Some(t) = tail_titles {
        return t;
    }
    let mut first = String::new();
    for l in head.split(|&b| b == b'\n') {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(l) else {
            continue;
        };
        if let Some(t) = v["customTitle"].as_str() {
            return t.to_string();
        }
        if first.is_empty() && v["type"] == "user" {
            if let Some(s) = v["message"]["content"].as_str() {
                first = crate::paths::head(s.trim(), 80).to_string();
            }
        }
    }
    first
}

/// The last `customTitle` in some bytes, if any.
pub fn last_title(bytes: &[u8]) -> Option<String> {
    let needle = b"\"customTitle\"";
    let mut t = None;
    for l in bytes.split(|&b| b == b'\n') {
        if l.windows(needle.len()).any(|w| w == needle) {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(l) {
                if let Some(s) = v["customTitle"].as_str() {
                    t = Some(s.to_string());
                }
            }
        }
    }
    t
}

/// Fork a Claude transcript: a copy under a fresh id (every line's
/// sessionId rewritten, as session_ops does for Conflict::NewId), named
/// "<title> (fork from <machine>, <date>)". Returns (new id, its path).
pub fn fork_claude(
    p: &Path,
    id: &str,
    title: &str,
    machine: &str,
    date: &str,
) -> Result<(String, PathBuf)> {
    let new_id = crate::session_ops::new_uuid();
    let text = std::fs::read(p)?;
    let from = format!("\"sessionId\":\"{id}\"");
    let to = format!("\"sessionId\":\"{new_id}\"");
    let mut out = replace_bytes(&text, from.as_bytes(), to.as_bytes());
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let name = format!(
        "{} (fork from {machine}, {date})",
        if title.is_empty() { "session" } else { title }
    );
    for v in [
        serde_json::json!({"type": "custom-title", "customTitle": name, "sessionId": new_id}),
        serde_json::json!({"type": "agent-name", "agentName": name, "sessionId": new_id}),
    ] {
        out.extend_from_slice(v.to_string().as_bytes());
        out.push(b'\n');
    }
    let np = p.with_file_name(format!("{new_id}.jsonl"));
    place(&np, &out)?;
    Ok((new_id, np))
}

/// Fork a Grok session folder: copied under a fresh id, its title marked.
pub fn fork_grok(dir: &Path, machine: &str, date: &str) -> Result<(String, PathBuf)> {
    let new_id = crate::session_ops::new_uuid();
    let nd = dir.with_file_name(&new_id);
    let mut files = vec![];
    collect_tree(dir, dir, &mut files);
    for (rel, src) in files {
        let mut b = std::fs::read(&src)?;
        if rel == "summary.json" {
            if let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(&b) {
                let t = v["generated_title"]
                    .as_str()
                    .unwrap_or("session")
                    .to_string();
                v["generated_title"] =
                    serde_json::json!(format!("{t} (fork from {machine}, {date})"));
                v["title_is_manual"] = serde_json::json!(true);
                if v.pointer("/info/id").is_some() {
                    v["info"]["id"] = serde_json::json!(new_id);
                }
                b = serde_json::to_vec_pretty(&v)?;
            }
        }
        place(&nd.join(&rel), &b)?;
    }
    Ok((new_id, nd))
}

fn collect_tree(root: &Path, d: &Path, out: &mut Vec<(String, PathBuf)>) {
    for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
        let p = e.path();
        let Ok(md) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if md.is_dir() {
            collect_tree(root, &p, out);
        } else if md.is_file() {
            if let Ok(r) = p.strip_prefix(root) {
                out.push((r.to_string_lossy().into_owned(), p));
            }
        }
    }
}

fn replace_bytes(hay: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while i < hay.len() {
        if hay[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn dir(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gt-sync-chunks-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Chunks of `data` from `from`, kept in a map standing in for a shard.
    fn chunk(
        store: &Mutex<HashMap<String, Vec<u8>>>,
        prev: &FileMan,
        data: &[u8],
        max: usize,
    ) -> FileMan {
        let mut f = prev.clone();
        let from = f.len as usize;
        let mut start = from;
        for cut in split_points(&data[from..], max) {
            let end = from + cut;
            let b = data[start..end].to_vec();
            let path = chunk_path("acct", "s1", "s1.jsonl", start as u64);
            f.chunks.push(Chunk {
                repo: "r".into(),
                path: path.clone(),
                off: start as u64,
                len: b.len() as u64,
                sha256: crate::sync::sha_hex(&b),
            });
            store.lock().unwrap().insert(path, b);
            start = end;
        }
        f.len = data.len() as u64;
        f.sha256 = crate::sync::sha_hex(data);
        f.name = "s1.jsonl".into();
        f
    }

    #[test]
    fn chunker_is_byte_exact_with_a_30_mb_line() {
        let store: Mutex<HashMap<String, Vec<u8>>> = Mutex::new(HashMap::new());
        let mut data = b"{\"a\":1}\n{\"b\":2}\n".to_vec();
        // One 30 MB line with no line end inside it.
        data.extend(std::iter::repeat_n(b'x', 30 << 20));
        data.extend_from_slice(b"\n{\"c\":3}\n");
        let f = chunk(&store, &FileMan::default(), &data, 25 << 20);
        assert!(f.chunks.len() >= 2 && f.chunks.iter().all(|c| c.len <= 25 << 20));
        assert_eq!(f.chunks[0].off, 0);
        // The first chunk ends at a line end; the long line is cut once.
        assert_eq!(f.chunks[0].len, 16);
        let read = |c: &Chunk| Ok(store.lock().unwrap()[&c.path].clone());
        let all = remote_bytes(&f, 0, &read).unwrap();
        assert_eq!(all, data, "concatenation is the file byte for byte");
        assert_eq!(crate::sync::sha_hex(&all), f.sha256);
        // Many small lines cut at line ends.
        let lines: Vec<u8> = (0..2000)
            .flat_map(|i| format!("{{\"n\":{i}}}\n").into_bytes())
            .collect();
        for c in split_points(&lines, 100) {
            assert_eq!(lines[c - 1], b'\n');
        }
        assert_eq!(split_points(&[], 10), Vec::<usize>::new());
    }

    #[test]
    fn extend_ahead_diverged() {
        let d = dir("cmp");
        let store: Mutex<HashMap<String, Vec<u8>>> = Mutex::new(HashMap::new());
        let read = |c: &Chunk| Ok(store.lock().unwrap()[&c.path].clone());
        let v1 = b"{\"l\":1}\n{\"l\":2}\n".to_vec();
        let mut v2 = v1.clone();
        v2.extend_from_slice(b"{\"l\":3}\n{\"l\":4}\n");
        let r1 = chunk(&store, &FileMan::default(), &v1, 8);
        let r2 = chunk(&store, &r1, &v2, 8);
        let p = d.join("s1.jsonl");
        // Equal.
        std::fs::write(&p, &v2).unwrap();
        assert_eq!(
            compare(&p, v2.len() as u64, &r2, &read).unwrap(),
            Cmp::Equal
        );
        // Local ahead of the remote.
        assert_eq!(
            compare(&p, v2.len() as u64, &r1, &read).unwrap(),
            Cmp::LocalAhead
        );
        // Remote ahead: extend locally, then equal.
        std::fs::write(&p, &v1).unwrap();
        assert_eq!(
            compare(&p, v1.len() as u64, &r2, &read).unwrap(),
            Cmp::RemoteAhead
        );
        let more = remote_bytes(&r2, v1.len() as u64, &read).unwrap();
        extend(&p, v1.len() as u64, &more).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), v2);
        // A local length inside a remote chunk: the straddling chunk is read.
        std::fs::write(&p, &v2[..20]).unwrap();
        assert_eq!(compare(&p, 20, &r2, &read).unwrap(), Cmp::RemoteAhead);
        // Diverged: same length, other bytes; or a changed middle.
        let mut other = v2.clone();
        other[3] = b'9';
        std::fs::write(&p, &other).unwrap();
        assert_eq!(
            compare(&p, other.len() as u64, &r2, &read).unwrap(),
            Cmp::Diverged
        );
        let mut longer = v1.clone();
        longer.extend_from_slice(b"{\"mine\":1}\n");
        std::fs::write(&p, &longer).unwrap();
        assert_eq!(
            compare(&p, longer.len() as u64, &r2, &read).unwrap(),
            Cmp::Diverged
        );
        // A fork gets a new id, its lines rewritten, the title marked.
        std::fs::write(&p, b"{\"sessionId\":\"s1\",\"x\":1}\n").unwrap();
        let (nid, np) = fork_claude(&p, "s1", "billing", "MacBook", "2026-10-25").unwrap();
        assert_ne!(nid, "s1");
        let t = std::fs::read_to_string(&np).unwrap();
        assert!(t.contains(&format!("\"sessionId\":\"{nid}\"")) && !t.contains("\"s1\""));
        assert_eq!(
            last_title(t.as_bytes()).unwrap(),
            "billing (fork from MacBook, 2026-10-25)"
        );
        assert!(p.exists(), "the original stays");
        let _ = std::fs::remove_dir_all(&d);
    }
}
