use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const MAX_BYTES: usize = 512 * 1024;
pub const GEN: &str = "gen:";

pub fn dir() -> PathBuf {
    crate::config::app_home().join("avatars")
}

pub fn is_generated(v: &str) -> bool {
    v.strip_prefix(GEN)
        .is_some_and(|n| !n.is_empty() && n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit()))
}

pub fn valid_file(name: &str) -> bool {
    let Some((stem, ext)) = name.split_once('.') else {
        return false;
    };
    stem.len() == 16
        && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && matches!(ext, "png" | "jpg" | "gif" | "webp" | "svg")
}

pub fn valid(v: &str) -> bool {
    is_generated(v) || valid_file(v)
}

pub fn clean(v: Option<String>) -> Option<String> {
    v.filter(|s| valid(s))
}

pub fn mime(name: &str) -> &'static str {
    match name.rsplit('.').next() {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn svg_ok(b: &[u8]) -> bool {
    let Ok(t) = std::str::from_utf8(b) else {
        return false;
    };
    let l = t.to_ascii_lowercase();
    if !l.contains("<svg") {
        return false;
    }
    for bad in [
        "<script",
        "<foreignobject",
        "<!entity",
        "<iframe",
        "<object",
        "<embed",
        "javascript:",
        "href=\"http",
        "href='http",
        "href=\"//",
        "url(http",
        "@import",
    ] {
        if l.contains(bad) {
            return false;
        }
    }
    let bytes = l.as_bytes();
    let mut i = 0;
    while let Some(p) = l[i..].find(" on") {
        let s = i + p + 3;
        let mut e = s;
        while e < bytes.len() && bytes[e].is_ascii_lowercase() {
            e += 1;
        }
        if e > s && bytes[e..].trim_ascii_start().starts_with(b"=") {
            return false;
        }
        i = s;
    }
    true
}

pub fn sniff(b: &[u8]) -> Option<&'static str> {
    match crate::assistant_images::media_type(b) {
        Some("image/png") => Some("png"),
        Some("image/jpeg") => Some("jpg"),
        Some("image/gif") => Some("gif"),
        Some("image/webp") => Some("webp"),
        _ => svg_ok(b).then_some("svg"),
    }
}

pub fn store(bytes: &[u8]) -> Result<String> {
    use crate::platform::PermissionsExt;
    if bytes.is_empty() {
        bail!("the image is empty");
    }
    if bytes.len() > MAX_BYTES {
        bail!("the image is over {} KB", MAX_BYTES / 1024);
    }
    let Some(ext) = sniff(bytes) else {
        bail!("the avatar must be a PNG, JPEG, GIF, WebP or plain SVG image");
    };
    let d = dir();
    std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    let sum = Sha256::digest(bytes);
    let stem: String = sum.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let name = format!("{stem}.{ext}");
    let p = d.join(&name);
    if !p.is_file() {
        crate::config::write_private(&p, bytes)?;
    }
    Ok(name)
}

pub fn read_stored(name: &str) -> Option<Vec<u8>> {
    if !valid_file(name) {
        return None;
    }
    std::fs::read(dir().join(name)).ok()
}

pub fn from_file(path: &Path, roots: &[PathBuf]) -> Result<String> {
    let real = std::fs::canonicalize(path)
        .with_context(|| format!("{} does not exist", path.display()))?;
    let inside = roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .any(|r| real.starts_with(&r));
    if !inside {
        bail!("the image must be inside this tab's folder");
    }
    let meta = std::fs::metadata(&real)?;
    if !meta.is_file() {
        bail!("{} is not a file", path.display());
    }
    if meta.len() as usize > MAX_BYTES {
        bail!("the image is over {} KB", MAX_BYTES / 1024);
    }
    store(&std::fs::read(&real)?)
}

const DIRS: &[&str] = &[
    "",
    "public",
    "static",
    "assets",
    "assets/images",
    "assets/img",
    "images",
    "img",
    "docs",
    "docs/assets",
    "docs/images",
    ".github",
    ".github/assets",
    "web",
    "web/public",
    "src/assets",
    "app/assets",
    "resources",
    "branding",
    "media",
];
const STEMS: &[&str] = &[
    "logo",
    "icon",
    "apple-touch-icon",
    "favicon",
    "avatar",
    "og-image",
    "og",
    "opengraph-image",
    "social",
];
const EXTS: &[&str] = &["png", "svg", "webp", "jpg", "jpeg"];

fn stem_rank(stem: &str) -> Option<usize> {
    let s = stem.to_ascii_lowercase();
    STEMS.iter().position(|k| {
        s.strip_prefix(k)
            .is_some_and(|rest| rest.chars().next().is_none_or(|c| !c.is_ascii_alphabetic()))
    })
}

fn roots_of(cwd: &Path) -> Vec<PathBuf> {
    let mut roots = vec![cwd.to_path_buf()];
    if let Some(top) = crate::project_git::repo_top(cwd) {
        if top != cwd {
            roots.push(top);
        }
    }
    roots
}

pub fn find_logo(cwd: &Path) -> Option<PathBuf> {
    let mut best: Option<((usize, usize, usize, usize), PathBuf)> = None;
    for (ri, root) in roots_of(cwd).iter().enumerate() {
        for (di, d) in DIRS.iter().enumerate() {
            let Ok(rd) = std::fs::read_dir(root.join(d)) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let (Some(stem), Some(ext)) = (
                    p.file_stem().and_then(|s| s.to_str()),
                    p.extension().and_then(|s| s.to_str()),
                ) else {
                    continue;
                };
                let (Some(sr), Some(er)) = (
                    stem_rank(stem),
                    EXTS.iter().position(|x| x.eq_ignore_ascii_case(ext)),
                ) else {
                    continue;
                };
                let Ok(m) = std::fs::metadata(&p) else {
                    continue;
                };
                if !m.is_file() || m.len() as usize > MAX_BYTES || m.len() == 0 {
                    continue;
                }
                let key = (sr, ri, di, er);
                if best.as_ref().is_none_or(|(k, _)| key < *k) {
                    best = Some((key, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

pub fn from_project(cwd: &Path) -> Result<String> {
    let Some(p) = find_logo(cwd) else {
        bail!("no logo, icon or favicon image found in this tab's folder (looked for logo, icon, apple-touch-icon, favicon, avatar and og images in the folder, public, assets, docs and .github)");
    };
    from_file(&p, &roots_of(cwd))
}

pub fn new_variation() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.subsec_nanos());
    format!("{GEN}{}", n % 100_000 + 1)
}
