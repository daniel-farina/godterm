#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};

pub const MAX_TOTAL: u64 = 64 << 20;

pub struct Packed {
    pub rel: String,

    pub path: PathBuf,
    pub bytes: Vec<u8>,

    pub gz: Option<Vec<u8>>,
}

pub struct Pack {
    pub id: String,

    pub files: Vec<Packed>,
}

pub fn walk(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = vec![];
    let Ok(real_root) = std::fs::canonicalize(root) else {
        return out;
    };
    let mut seen = vec![real_root.clone()];
    walk_in(&real_root, root, root, &mut seen, &mut out);
    out.sort();
    out
}

fn walk_in(
    real_root: &Path,
    root: &Path,
    dir: &Path,
    seen: &mut Vec<PathBuf>,
    out: &mut Vec<(String, PathBuf)>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(real) = std::fs::canonicalize(&p) else {
            continue;
        };
        if !real.starts_with(real_root) {
            continue;
        }
        if real.is_dir() {
            if name == "dev" || seen.contains(&real) {
                continue;
            }
            seen.push(real);
            walk_in(real_root, root, &p, seen, out);
        } else if real.is_file() {
            let Ok(rel) = p.strip_prefix(root) else {
                continue;
            };
            out.push((rel.to_string_lossy().replace('\\', "/"), real));
        }
    }
}

pub fn pack(root: &Path) -> std::io::Result<Pack> {
    let mut all = vec![];
    let mut total = 0u64;
    for (rel, path) in walk(root) {
        let bytes = std::fs::read(&path)?;
        total += bytes.len() as u64;
        if total > MAX_TOTAL {
            return Err(std::io::Error::other(format!(
                "{} holds over {} MB",
                root.display(),
                MAX_TOTAL >> 20
            )));
        }
        all.push((rel, path, bytes));
    }
    Ok(pack_files(all))
}

pub fn pack_files(all: Vec<(String, PathBuf, Vec<u8>)>) -> Pack {
    use sha2::Digest;

    let mut hash = sha2::Sha256::new();
    for (rel, _, bytes) in &all {
        hash.update(rel.as_bytes());
        hash.update([0]);
        hash.update(bytes);
    }
    let h = hash.finalize();
    let id: String = h[..6].iter().map(|b| format!("{b:02x}")).collect();
    let files = all
        .into_iter()
        .map(|(rel, path, bytes)| {
            let bytes = versioned(&rel, &bytes, &id).unwrap_or(bytes);
            let gz = gzip(&rel, &bytes);
            Packed {
                rel,
                path,
                bytes,
                gz,
            }
        })
        .collect();
    Pack { id, files }
}

pub fn gzip(rel: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    let ext = Path::new(rel).extension().and_then(|e| e.to_str());
    let text = matches!(
        ext,
        Some("html" | "js" | "mjs" | "css" | "json" | "svg" | "map" | "txt" | "webmanifest")
    );
    if !text || bytes.len() <= 512 {
        return None;
    }
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(bytes).ok()?;
    let z = e.finish().ok()?;
    (z.len() < bytes.len() * 9 / 10).then_some(z)
}

pub fn versioned(rel: &str, bytes: &[u8], id: &str) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let tag = |path: &str| format!("{path}?v={id}");
    if rel == "sw.js" {
        return Some(text.replace("__BUILD__", id).into_bytes());
    }
    let mut out = String::with_capacity(text.len() + 256);
    let mut rest = text;
    if rel == "index.html" {
        while let Some(i) = rest.find(['s', 'h']) {
            let (head, tail) = rest.split_at(i);
            out.push_str(head);
            let attr = ["src=\"", "href=\""].iter().find(|a| tail.starts_with(**a));
            let Some(attr) = attr else {
                out.push_str(&tail[..1]);
                rest = &tail[1..];
                continue;
            };
            out.push_str(attr);
            let after = &tail[attr.len()..];
            let end = after.find('"').unwrap_or(after.len());
            let v = &after[..end];
            let ours = !v.is_empty()
                && !v.contains(':')
                && !v.starts_with('#')
                && !v.starts_with("//")
                && !v.contains('?');
            out.push_str(&if ours { tag(v) } else { v.to_string() });
            rest = &after[end..];
        }
        out.push_str(rest);
        return Some(out.into_bytes());
    }
    if rel.starts_with("js/") && rel.ends_with(".js") {
        while let Some(i) = rest.find("'./") {
            let (head, tail) = rest.split_at(i + 1);
            out.push_str(head);
            let end = tail.find('\'').unwrap_or(tail.len());
            let v = &tail[..end];
            out.push_str(&if v.ends_with(".js") {
                tag(v)
            } else {
                v.to_string()
            });
            rest = &tail[end..];
        }
        out.push_str(rest);
        return Some(out.into_bytes());
    }
    None
}
