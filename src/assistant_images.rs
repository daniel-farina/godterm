//! Images the user attaches in the assistant panel: read from the
//! clipboard (Ctrl-V, or an empty paste, which is what Cmd-V of an image
//! gives a terminal) or pasted and dragged image file paths. Each is kept
//! in `~/.godterm/assistant/images` (owner only, pruned after 7 days),
//! shown in the input as a chip ("[image 1: 1280x720]"), sent to the
//! assistant's own model as an image block, and stays referable by its
//! number for 30 minutes so the assistant can pass it on to a tab
//! (send_prompt or open_tab with images). Nothing is uploaded anywhere
//! else.
//!
//! Claude Code tabs get the image as its path on a line of its own: a
//! pasted line that is an image file path is attached as "[Image #1]"
//! (Claude Code 2.1's paste handler), and else it reads the file by path.
//! Grok tabs get "Image: /path" lines.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};

/// The long side images are scaled down to before the model sees them.
pub const MAX_SIDE: u32 = 1568;
/// The most bytes one image may have for the model (its base64 stays
/// under the API's 5 MB per image).
pub const MAX_BYTES: usize = 3_700_000;
/// Saved images older than this are removed.
pub const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);
/// How long an attachment stays referable by its number.
pub const REFER_FOR: Duration = Duration::from_secs(30 * 60);
/// File types taken as images when their path is pasted or dragged.
pub const EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "heic"];

/// One attached image.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    /// Its number in this conversation ("image 1").
    pub id: u32,
    /// The saved copy (absolute).
    pub path: PathBuf,
    pub w: u32,
    pub h: u32,
    pub at: Instant,
}

impl Attachment {
    fn size(&self) -> String {
        if self.w > 0 && self.h > 0 {
            format!("{}x{}", self.w, self.h)
        } else {
            "size unknown".into()
        }
    }

    /// The chip in the input: "[image 1: 1280x720]".
    pub fn chip(&self) -> String {
        format!("[image {}: {}]", self.id, self.size())
    }

    /// "image 1 (1280x720)".
    pub fn label(&self) -> String {
        format!("image {} ({})", self.id, self.size())
    }

    pub fn fresh(&self) -> bool {
        self.at.elapsed() < REFER_FOR
    }
}

pub fn dir() -> PathBuf {
    crate::assistant::dir().join("images")
}

/// The images folder, made owner only.
fn ensure_dir() -> Result<PathBuf> {
    use crate::platform::PermissionsExt;
    let d = dir();
    std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    Ok(d)
}

/// A new file name in the images folder: `<timestamp>-<n>.<ext>`.
fn new_file(ext: &str) -> Result<PathBuf> {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let d = ensure_dir()?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    loop {
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = d.join(format!("{stamp}-{n}.{ext}"));
        if !p.exists() {
            return Ok(p);
        }
    }
}

/// What the clipboard holds that can be attached.
// Only the macOS reader builds File, and Windows builds neither yet.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
pub enum ClipImage {
    /// Image data, as PNG.
    Png(Vec<u8>),
    /// A copied image file (Finder's Copy).
    File(PathBuf),
}

/// The system clipboard (a fake in tests: they never touch the real one).
pub trait Clipboard: Send {
    /// Its image, if it holds one.
    fn image(&self) -> Result<Option<ClipImage>>;
}

/// The real clipboard: NSPasteboard through osascript on macOS (PNG,
/// TIFF or JPEG, made PNG), wl-paste or xclip on Linux. Windows: not yet
/// (future work).
pub struct SystemClipboard;

#[cfg(target_os = "macos")]
const CLIP_SCRIPT: &[&str] = &[
    "on run argv",
    "set out to item 1 of argv",
    "try",
    "set u to the clipboard as «class furl»",
    "return \"file:\" & (POSIX path of u)",
    "end try",
    "set kind to \"none\"",
    "try",
    "set d to the clipboard as «class PNGf»",
    "set kind to \"png\"",
    "on error",
    "try",
    "set d to the clipboard as «class TIFF»",
    "set kind to \"tiff\"",
    "on error",
    "try",
    "set d to the clipboard as «class JPEG»",
    "set kind to \"jpeg\"",
    "end try",
    "end try",
    "end try",
    "if kind is \"none\" then return kind",
    "set h to open for access (POSIX file out) with write permission",
    "set eof h to 0",
    "write d to h",
    "close access h",
    "return kind",
    "end run",
];

impl Clipboard for SystemClipboard {
    #[cfg(target_os = "macos")]
    fn image(&self) -> Result<Option<ClipImage>> {
        let tmp = new_file("clip")?;
        let mut c = std::process::Command::new("osascript");
        for l in CLIP_SCRIPT {
            c.arg("-e").arg(l);
        }
        let out = c.arg(&tmp).output().context("running osascript")?;
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let got = (|| -> Result<Option<ClipImage>> {
            if let Some(p) = said.strip_prefix("file:") {
                let p = PathBuf::from(p);
                return Ok(is_image_path(&p).then_some(ClipImage::File(p)));
            }
            match said.as_str() {
                "png" => Ok(Some(ClipImage::Png(std::fs::read(&tmp)?))),
                "tiff" | "jpeg" => {
                    let png = tmp.with_extension("png");
                    let ok = std::process::Command::new("sips")
                        .args(["-s", "format", "png"])
                        .arg(&tmp)
                        .arg("--out")
                        .arg(&png)
                        .output()
                        .is_ok_and(|o| o.status.success());
                    let bytes = std::fs::read(&png).ok();
                    let _ = std::fs::remove_file(&png);
                    match bytes {
                        Some(b) if ok => Ok(Some(ClipImage::Png(b))),
                        _ => anyhow::bail!("could not convert the clipboard's {said} image"),
                    }
                }
                _ => Ok(None),
            }
        })();
        let _ = std::fs::remove_file(&tmp);
        got
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn image(&self) -> Result<Option<ClipImage>> {
        let tries: &[(&str, &[&str])] = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            &[("wl-paste", &["--no-newline", "--type", "image/png"])]
        } else {
            &[(
                "xclip",
                &["-selection", "clipboard", "-t", "image/png", "-o"],
            )]
        };
        for (bin, args) in tries {
            if let Ok(o) = std::process::Command::new(bin).args(*args).output() {
                if o.status.success() && media_type(&o.stdout) == Some("image/png") {
                    return Ok(Some(ClipImage::Png(o.stdout)));
                }
            }
        }
        Ok(None)
    }

    // Windows: reading clipboard images is future work.
    #[cfg(windows)]
    fn image(&self) -> Result<Option<ClipImage>> {
        Ok(None)
    }
}

/// An image's type from its first bytes.
pub fn media_type(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Width and height from the image's header (PNG, GIF, JPEG, WebP).
pub fn dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let be16 =
        |i: usize| -> Option<u32> { Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32) };
    let le16 =
        |i: usize| -> Option<u32> { Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32) };
    let le24 = |i: usize| -> Option<u32> {
        Some(u32::from_le_bytes([
            *b.get(i)?,
            *b.get(i + 1)?,
            *b.get(i + 2)?,
            0,
        ]))
    };
    match media_type(b)? {
        "image/png" => {
            let w = u32::from_be_bytes(b.get(16..20)?.try_into().ok()?);
            let h = u32::from_be_bytes(b.get(20..24)?.try_into().ok()?);
            Some((w, h))
        }
        "image/gif" => Some((le16(6)?, le16(8)?)),
        "image/jpeg" => {
            let mut i = 2;
            while i + 9 < b.len() {
                if b[i] != 0xFF {
                    return None;
                }
                let m = b[i + 1];
                if m == 0xFF {
                    i += 1;
                    continue;
                }
                if (0xC0..=0xCF).contains(&m) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
                    return Some((be16(i + 7)?, be16(i + 5)?));
                }
                i += 2 + be16(i + 2)? as usize;
            }
            None
        }
        "image/webp" => match b.get(12..16)? {
            b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
            b"VP8 " => Some((le16(26)? & 0x3FFF, le16(28)? & 0x3FFF)),
            b"VP8L" => {
                let v = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                Some(((v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Width and height of an image file (its header, else sips on macOS).
pub fn file_dimensions(p: &Path) -> Option<(u32, u32)> {
    if let Some(d) = std::fs::read(p).ok().and_then(|b| dimensions(&b)) {
        return Some(d);
    }
    if !cfg!(target_os = "macos") {
        return None;
    }
    let o = std::process::Command::new("sips")
        .args(["-g", "pixelWidth", "-g", "pixelHeight"])
        .arg(p)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&o.stdout);
    let num = |k: &str| -> Option<u32> {
        s.lines()
            .find_map(|l| l.trim().strip_prefix(k))
            .and_then(|v| v.trim().parse().ok())
    };
    Some((num("pixelWidth:")?, num("pixelHeight:")?))
}

/// The size that fits `max` on the long side (never larger), as sips -Z
/// makes it.
#[cfg(test)]
pub fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= max || long == 0 {
        return (w, h);
    }
    let s = |x: u32| ((x as u64 * max as u64 + long as u64 / 2) / long as u64).max(1) as u32;
    (s(w), s(h))
}

/// Whether a path names an image by its extension.
pub fn is_image_path(p: &Path) -> bool {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| EXTS.contains(&e.as_str()))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The words of a paste, shell style: quotes group, a backslash escapes.
fn shell_words(text: &str) -> Option<Vec<String>> {
    let mut words = vec![];
    let mut cur = String::new();
    let mut have = false;
    let mut quote: Option<char> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => cur.push(c),
            // Windows paths use the backslash as a separator: literal there.
            (_, '\\') if !cfg!(windows) => {
                cur.push(chars.next()?);
                have = true;
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                have = true;
            }
            (None, c) if c.is_whitespace() => {
                if have || !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                }
                have = false;
            }
            (None, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        return None;
    }
    if have || !cur.is_empty() {
        words.push(cur);
    }
    Some(words)
}

/// One pasted word as a path: file:// URLs decoded, ~ expanded.
fn word_path(w: &str) -> Option<PathBuf> {
    let w = match w.strip_prefix("file://") {
        // file://localhost/x or file:///x
        Some(rest) => {
            let d = percent_decode(rest.strip_prefix("localhost").unwrap_or(rest));
            // file:///C:/x names C:/x on Windows.
            let drive = d.as_bytes();
            if cfg!(windows) && drive.len() > 2 && drive[0] == b'/' && drive[1].is_ascii_alphabetic() && drive[2] == b':' {
                d[1..].to_string()
            } else {
                d
            }
        }
        None => w.to_string(),
    };
    let p = crate::config::expand_tilde(&w);
    p.is_absolute().then_some(p)
}

/// The image paths a paste consists of (quoted, backslash escaped or
/// file:// forms, one or several), if it is nothing but image paths.
/// Only the form is checked here; see `pasted_images`.
pub fn paste_paths(text: &str) -> Option<Vec<PathBuf>> {
    let words = shell_words(text.trim())?;
    if words.is_empty() {
        return None;
    }
    let paths: Vec<PathBuf> = words
        .iter()
        .map(|w| word_path(w))
        .collect::<Option<Vec<_>>>()?;
    paths.iter().all(|p| is_image_path(p)).then_some(paths)
}

/// The existing image files a paste names, if it names nothing else (a
/// path with plain spaces in it counts too).
pub fn pasted_images(text: &str) -> Option<Vec<PathBuf>> {
    if let Some(v) = paste_paths(text).filter(|v| v.iter().all(|p| p.is_file())) {
        return Some(v);
    }
    let t = text.trim();
    let whole = word_path(t)?;
    (!t.contains('\n') && is_image_path(&whole) && whole.is_file()).then(|| vec![whole])
}

/// Save PNG bytes (from the clipboard): its path and size.
pub fn save_png(bytes: &[u8]) -> Result<(PathBuf, u32, u32)> {
    if media_type(bytes) != Some("image/png") {
        anyhow::bail!("the clipboard image is not a PNG");
    }
    let (w, h) = dimensions(bytes).unwrap_or((0, 0));
    let p = new_file("png")?;
    crate::config::write_private(&p, bytes)?;
    Ok((p, w, h))
}

/// Save image bytes (PNG, JPEG, GIF or WebP, as a browser pastes them):
/// the path and size.
pub fn save_bytes(bytes: &[u8]) -> Result<(PathBuf, u32, u32)> {
    let ext = match media_type(bytes) {
        Some("image/jpeg") => "jpg",
        Some(mt) => mt.trim_start_matches("image/"),
        None => anyhow::bail!("not a PNG, JPEG, GIF or WebP image"),
    };
    let (w, h) = dimensions(bytes).unwrap_or((0, 0));
    let p = new_file(ext)?;
    crate::config::write_private(&p, bytes)?;
    Ok((p, w, h))
}

/// Standard base64 (padding optional, whitespace skipped) back to bytes;
/// None on any other character.
pub fn unbase64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b' ' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        acc = ((acc << 6) | v as u32) & 0xff_ffff;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Copy an image file into the images folder (HEIC made PNG): the copy
/// and its size.
pub fn import_file(src: &Path) -> Result<(PathBuf, u32, u32)> {
    let bytes = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
    let p = match media_type(&bytes) {
        Some(mt) => {
            let ext = match mt {
                "image/jpeg" => "jpg",
                other => other.trim_start_matches("image/"),
            };
            let p = new_file(ext)?;
            crate::config::write_private(&p, &bytes)?;
            p
        }
        // HEIC and the like: made PNG with sips.
        None => {
            let p = new_file("png")?;
            let ok = cfg!(target_os = "macos")
                && std::process::Command::new("sips")
                    .args(["-s", "format", "png"])
                    .arg(src)
                    .arg("--out")
                    .arg(&p)
                    .output()
                    .is_ok_and(|o| o.status.success());
            if !ok {
                let _ = std::fs::remove_file(&p);
                anyhow::bail!("{} is not an image GodTerm can read", src.display());
            }
            set_private(&p);
            p
        }
    };
    let (w, h) = file_dimensions(&p).unwrap_or((0, 0));
    Ok((p, w, h))
}

fn set_private(p: &Path) {
    use crate::platform::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}

/// An image block for the assistant's model.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelImage {
    pub media_type: String,
    /// Base64.
    pub data: String,
    pub w: u32,
    pub h: u32,
}

/// Run sips to make `dst` from `src` (macOS).
fn sips(src: &Path, dst: &Path, args: &[&str]) -> Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("too large, and there is no image resizer on this system");
    }
    let o = std::process::Command::new("sips")
        .args(args)
        .arg(src)
        .arg("--out")
        .arg(dst)
        .output()
        .context("running sips")?;
    if !o.status.success() {
        anyhow::bail!("sips failed: {}", String::from_utf8_lossy(&o.stderr).trim());
    }
    set_private(dst);
    Ok(())
}

/// The image as the model gets it: at most MAX_SIDE on the long side and
/// MAX_BYTES (scaled down, then JPEG if still too big; macOS sips).
pub fn for_model(p: &Path) -> Result<ModelImage> {
    let mut bytes = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
    let (mut w, mut h) = dimensions(&bytes).unwrap_or((0, 0));
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = p.parent().unwrap_or(Path::new("."));
    if media_type(&bytes).is_none() || w.max(h) > MAX_SIDE {
        let dst = dir.join(format!("{stem}.send.png"));
        let max = MAX_SIDE.to_string();
        sips(p, &dst, &["-Z", &max, "-s", "format", "png"])?;
        bytes = std::fs::read(&dst)?;
        (w, h) = dimensions(&bytes).unwrap_or((0, 0));
    }
    if bytes.len() > MAX_BYTES {
        let src = dir.join(format!("{stem}.send.src"));
        std::fs::write(&src, &bytes)?;
        let dst = dir.join(format!("{stem}.send.jpg"));
        let r = sips(
            &src,
            &dst,
            &["-s", "format", "jpeg", "-s", "formatOptions", "80"],
        );
        let _ = std::fs::remove_file(&src);
        r?;
        bytes = std::fs::read(&dst)?;
        if bytes.len() > MAX_BYTES {
            anyhow::bail!("still too large after shrinking");
        }
    }
    let media_type = media_type(&bytes)
        .context("not an image the model takes")?
        .to_string();
    Ok(ModelImage {
        media_type,
        data: base64(&bytes),
        w,
        h,
    })
}

/// Standard base64 with padding.
pub fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

/// Remove saved images older than `keep`: how many went.
pub fn prune_in(d: &Path, keep: Duration, now: SystemTime) -> usize {
    let Ok(rd) = std::fs::read_dir(d) else {
        return 0;
    };
    let mut n = 0;
    for e in rd.flatten() {
        let Ok(m) = e.metadata() else { continue };
        let old = m
            .modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > keep);
        if m.is_file() && old && std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

/// Prune the images folder (7 days).
pub fn prune() -> usize {
    prune_in(&dir(), KEEP, SystemTime::now())
}

/// A prompt for a tab with its images: Claude Code takes a line that is
/// an image path as an attachment (or reads it by path); Grok gets
/// "Image: /path" lines.
pub fn tab_text(text: &str, paths: &[PathBuf], grok: bool) -> String {
    if paths.is_empty() {
        return text.to_string();
    }
    let lines: Vec<String> = paths
        .iter()
        .map(|p| {
            if grok {
                format!("Image: {}", p.display())
            } else {
                p.display().to_string()
            }
        })
        .collect();
    format!("{}\n\n{}", text.trim_end(), lines.join("\n"))
}

fn image_line(l: &str) -> bool {
    let l = l.trim();
    let l = l.strip_prefix("Image: ").unwrap_or(l);
    let p = Path::new(l);
    !l.is_empty() && p.is_absolute() && is_image_path(p)
}

/// A prompt without the image lines `tab_text` added (what a tab's
/// transcript shows as its text; receipts look for that).
pub fn strip_image_lines(text: &str) -> &str {
    let mut end = text.trim_end().len();
    loop {
        let head = &text[..end];
        let (rest, last) = match head.rfind('\n') {
            Some(i) => (&head[..i], &head[i + 1..]),
            None => ("", head),
        };
        if !image_line(last) {
            break;
        }
        end = rest.trim_end().len();
        if end == 0 {
            return text;
        }
    }
    &text[..end]
}

#[cfg(test)]
pub mod testing {
    //! A minimal PNG writer (stored deflate) and a fake clipboard.
    use super::*;

    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }

    /// A gray RGB PNG of w x h.
    pub fn png(w: u32, h: u32) -> Vec<u8> {
        let mut raw = Vec::with_capacity(((w * 3 + 1) * h) as usize);
        for y in 0..h {
            raw.push(0);
            for x in 0..w {
                raw.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 128]);
            }
        }
        let mut z = vec![0x78, 0x01];
        let chunks: Vec<&[u8]> = raw.chunks(65535).collect();
        for (i, c) in chunks.iter().enumerate() {
            z.push((i + 1 == chunks.len()) as u8);
            let n = c.len() as u16;
            z.extend_from_slice(&n.to_le_bytes());
            z.extend_from_slice(&(!n).to_le_bytes());
            z.extend_from_slice(c);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &x in &raw {
            a = (a + x as u32) % 65521;
            b = (b + a) % 65521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc32(&body).to_be_bytes());
        };
        let mut ihdr = vec![];
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &z);
        chunk(b"IEND", &[]);
        out
    }

    /// A clipboard that holds what the test put there, and counts reads.
    #[derive(Clone, Default)]
    pub struct FakeClipboard {
        pub holds: std::sync::Arc<std::sync::Mutex<Option<ClipImage>>>,
        pub reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Clipboard for FakeClipboard {
        fn image(&self) -> Result<Option<ClipImage>> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.holds.lock().unwrap().clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_goes_both_ways() {
        for n in 0..40usize {
            let b: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(unbase64(&base64(&b)), Some(b), "{n} bytes");
        }
        assert_eq!(unbase64("aGk"), Some(b"hi".to_vec()), "no padding");
        assert_eq!(unbase64("aG\nk="), Some(b"hi".to_vec()), "a line break");
        assert_eq!(unbase64("a-b"), None);
    }

    #[test]
    fn path_paste_forms() {
        let p = |s: &str| {
            paste_paths(s).map(|v| {
                v.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
            })
        };
        if cfg!(windows) {
            // A dragged Windows path: plain, quoted, a file URL, several.
            assert_eq!(p(r"C:\Users\me\a.png"), Some(vec![r"C:\Users\me\a.png".into()]));
            assert_eq!(
                p(r#"  "C:\Users\me\My Shot.PNG"  "#),
                Some(vec![r"C:\Users\me\My Shot.PNG".into()])
            );
            assert_eq!(
                p("'D:\\x y\\z.jpg'"),
                Some(vec![r"D:\x y\z.jpg".into()])
            );
            assert_eq!(
                p("file:///C:/Users/me/a%20b.webp"),
                Some(vec!["C:/Users/me/a b.webp".into()])
            );
            assert_eq!(
                p("C:\\a.png \"C:\\b c.heic\"\n'C:\\d.jpg'"),
                Some(vec![r"C:\a.png".into(), r"C:\b c.heic".into(), r"C:\d.jpg".into()])
            );
            for t in [r"C:\a.txt", "a.png", r"look at C:\a.png", r"C:\a.png and", "", "   ", r"'C:\a.png", "/tmp/a.png"] {
                assert_eq!(paste_paths(t), None, "{t:?}");
            }
            return;
        }
        assert_eq!(p("/tmp/a.png"), Some(vec!["/tmp/a.png".into()]));
        assert_eq!(
            p("  '/tmp/My Shot.PNG'  "),
            Some(vec!["/tmp/My Shot.PNG".into()])
        );
        assert_eq!(p("\"/tmp/x y.jpg\""), Some(vec!["/tmp/x y.jpg".into()]));
        assert_eq!(
            p("/tmp/Screen\\ Shot\\ 1.jpeg"),
            Some(vec!["/tmp/Screen Shot 1.jpeg".into()])
        );
        assert_eq!(
            p("file:///tmp/a%20b.webp"),
            Some(vec!["/tmp/a b.webp".into()])
        );
        assert_eq!(
            p("file://localhost/tmp/c.gif"),
            Some(vec!["/tmp/c.gif".into()])
        );
        assert_eq!(
            p("/tmp/a.png /tmp/b\\ c.heic\n'/tmp/d.jpg'"),
            Some(vec![
                "/tmp/a.png".into(),
                "/tmp/b c.heic".into(),
                "/tmp/d.jpg".into()
            ])
        );
        // Not images, not paths, or text around them: pasted as text.
        for t in [
            "/tmp/a.txt",
            "a.png",
            "look at /tmp/a.png",
            "/tmp/a.png and",
            "",
            "   ",
            "'/tmp/a.png",
        ] {
            assert_eq!(paste_paths(t), None, "{t:?}");
        }
    }

    #[test]
    fn pasted_files_must_exist() {
        let d = std::env::temp_dir().join(format!("godterm-img-paste-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let a = d.join("Screen Shot.png");
        std::fs::write(&a, testing::png(4, 3)).unwrap();
        let txt = d.join("notes.txt");
        std::fs::write(&txt, "x").unwrap();
        // Unix pastes escape spaces with a backslash; Windows ones quote.
        let esc = if cfg!(windows) {
            format!("\"{}\"", a.display())
        } else {
            a.display().to_string().replace(' ', "\\ ")
        };
        assert_eq!(pasted_images(&esc), Some(vec![a.clone()]));
        assert_eq!(
            pasted_images(&format!("'{}'", a.display())),
            Some(vec![a.clone()])
        );
        // A plain path with spaces (no quoting) still counts.
        assert_eq!(
            pasted_images(&a.display().to_string()),
            Some(vec![a.clone()])
        );
        assert_eq!(
            pasted_images(&d.join("gone.png").display().to_string()),
            None
        );
        assert_eq!(pasted_images(&txt.display().to_string()), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn headers_and_fit() {
        let png = testing::png(1280, 720);
        assert_eq!(media_type(&png), Some("image/png"));
        assert_eq!(dimensions(&png), Some((1280, 720)));
        let gif = [b"GIF89a".as_slice(), &[0x40, 0x01, 0xF0, 0x00]].concat();
        assert_eq!(dimensions(&gif), Some((320, 240)));
        // JPEG: SOI, an APP0 segment, then SOF0 with 480 x 640.
        let jpg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x02,
            0x80, 0x01, 0xE0, 0x03, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(dimensions(&jpg), Some((480, 640)));
        assert_eq!(fit(3000, 1500, 1568), (1568, 784));
        assert_eq!(fit(1000, 4000, 1568), (392, 1568));
        assert_eq!(fit(800, 600, 1568), (800, 600));
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }

    #[test]
    fn tab_text_forms_and_strip() {
        // Absolute on every OS (a Windows path has a drive).
        let r = if cfg!(windows) { "C:" } else { "" };
        let (one, two) = (format!("{r}/h/.godterm/assistant/images/1.png"), format!("{r}/h/b.jpg"));
        let ps = vec![PathBuf::from(&one), PathBuf::from(&two)];
        let c = tab_text("fix the button", &ps, false);
        assert_eq!(c, format!("fix the button\n\n{one}\n{two}"));
        let g = tab_text("fix the button", &ps, true);
        assert_eq!(g, format!("fix the button\n\nImage: {one}\nImage: {two}"));
        assert_eq!(strip_image_lines(&c), "fix the button");
        assert_eq!(strip_image_lines(&g), "fix the button");
        assert_eq!(strip_image_lines("no images"), "no images");
        assert_eq!(tab_text("x", &[], false), "x");
        // Only a path: kept (nothing else to look for).
        let only = format!("{r}/a.png");
        assert_eq!(strip_image_lines(&only), only);
    }

    #[test]
    fn downscale_for_the_model() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("godterm-img-down-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        crate::config::testing::set_home(&home);
        let (small, w, h) = save_png(&testing::png(64, 32)).unwrap();
        assert_eq!((w, h), (64, 32));
        let m = for_model(&small).unwrap();
        assert_eq!((m.media_type.as_str(), m.w, m.h), ("image/png", 64, 32));
        assert_eq!(m.data, base64(&std::fs::read(&small).unwrap()));
        {
            use crate::platform::PermissionsExt;
            let mode = std::fs::metadata(&small).unwrap().permissions().mode();
            // Windows has no mode bits: the file is the user's own, writable.
            let want = if cfg!(windows) { 0o644 } else { 0o600 };
            assert_eq!(mode & 0o777, want);
        }
        if cfg!(target_os = "macos") {
            let (big, _, _) = save_png(&testing::png(2400, 1200)).unwrap();
            let m = for_model(&big).unwrap();
            assert_eq!((m.w, m.h), (1568, 784), "scaled to fit");
            assert!(m.data.len() < 5_000_000);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn prunes_old_images() {
        let d = std::env::temp_dir().join(format!("godterm-img-prune-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let old = d.join("old.png");
        let new = d.join("new.png");
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&new, b"y").unwrap();
        let f = std::fs::File::options().write(true).open(&old).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(8 * 24 * 3600))
            .unwrap();
        drop(f);
        assert_eq!(prune_in(&d, KEEP, SystemTime::now()), 1);
        assert!(!old.exists() && new.exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
