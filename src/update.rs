//! Self update, like Claude Code's native installer and Grok Build: check
//! GitHub Releases, download the platform's archive in the background,
//! verify it (SHA256SUMS, its minisign signature, and on macOS the Developer
//! ID signature), and install it as a new version in ~/.godterm/versions
//! with one atomic rename of the active link, keeping the previous version
//! for `godterm update --rollback`. Installs owned by a package manager
//! (Homebrew, apt, dnf) are never touched: the user gets the command.
//!
//! Layout:
//!   ~/.godterm/updates/state.json      the check cache (ETag, last release, back off, skipped)
//!   ~/.godterm/updates/<v>/            a download being verified (resumable .part)
//!   ~/.godterm/versions/<v>/godterm    installed versions
//!   ~/.godterm/versions/state.json     what is active, what was before (rollback)

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const REPO: &str = "daniel-farina/godterm";
pub const API: &str = "https://api.github.com";
/// The minisign public key whose secret signs each release's SHA256SUMS
/// (scripts/update-signing/minisign.py; the secret key never leaves the
/// release machine).
pub const UPDATE_PUBKEY: &str = "RWTNzCFSrTk4Erhp4n8mN0kO3VgXNUYQliJVGM6jC0jaVe07tPVATFcT";
/// The Developer ID team that signs the macOS builds.
pub const TEAM_ID: &str = "ST6RKUS2KP";
/// Hosts a download may come from (each redirect is checked too).
const HOSTS: &[&str] = &[
    "github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

// ---------- versions ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

impl Version {
    /// "v0.2.2", "0.2.2-beta.1", "0.3" (patch 0); build metadata ignored.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.to_string())),
            Some(_) => return None,
            None => (s, None),
        };
        let mut it = core.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        let patch = match it.next() {
            Some(p) => p.parse().ok()?,
            None => 0,
        };
        if it.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is semver")
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        (self.major, self.minor, self.patch)
            .cmp(&(o.major, o.minor, o.patch))
            .then_with(|| match (&self.pre, &o.pre) {
                (None, None) => Equal,
                (None, Some(_)) => Greater,
                (Some(_), None) => Less,
                (Some(a), Some(b)) => {
                    let mut x = a.split('.');
                    let mut y = b.split('.');
                    loop {
                        match (x.next(), y.next()) {
                            (None, None) => return Equal,
                            (None, Some(_)) => return Less,
                            (Some(_), None) => return Greater,
                            (Some(p), Some(q)) => {
                                let c = match (p.parse::<u64>(), q.parse::<u64>()) {
                                    (Ok(m), Ok(n)) => m.cmp(&n),
                                    (Ok(_), Err(_)) => Less,
                                    (Err(_), Ok(_)) => Greater,
                                    _ => p.cmp(q),
                                };
                                if c != Equal {
                                    return c;
                                }
                            }
                        }
                    }
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

// ---------- releases ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub body: String,
    pub page: String,
    pub prerelease: bool,
    pub published: Option<String>,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn from_json(v: &Value) -> Option<Release> {
        if v["draft"].as_bool() == Some(true) {
            return None;
        }
        let tag = v["tag_name"].as_str()?.to_string();
        Version::parse(&tag)?;
        Some(Release {
            tag,
            body: v["body"].as_str().unwrap_or("").to_string(),
            page: v["html_url"].as_str().unwrap_or("").to_string(),
            prerelease: v["prerelease"].as_bool().unwrap_or(false),
            published: v["published_at"].as_str().map(str::to_string),
            assets: v["assets"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            Some(Asset {
                                name: x["name"].as_str()?.to_string(),
                                url: x["browser_download_url"].as_str()?.to_string(),
                                size: x["size"].as_u64().unwrap_or(0),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    pub fn version(&self) -> Version {
        Version::parse(&self.tag).unwrap_or(Version {
            major: 0,
            minor: 0,
            patch: 0,
            pre: None,
        })
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// The version-less alias, or the same file with the version in its
    /// name (releases before the aliases).
    pub fn pick(&self, alias: &str) -> Option<&Asset> {
        let v = self.version().to_string();
        let candidates = [
            alias.to_string(),
            alias.replacen("godterm-", &format!("godterm-{v}-"), 1),
            alias.replacen("GodTerm-", &format!("GodTerm-{v}-"), 1),
        ];
        candidates.iter().find_map(|c| self.asset(c))
    }

    /// The release notes as plain lines (markdown marks dropped).
    pub fn notes(&self, max: usize) -> Vec<String> {
        self.body
            .lines()
            .map(|l| {
                l.trim()
                    .trim_start_matches('#')
                    .trim_start_matches("- ")
                    .trim_start_matches("* ")
                    .replace("**", "")
                    .replace('`', "")
                    .trim()
                    .to_string()
            })
            .filter(|l| !l.is_empty())
            .take(max)
            .collect()
    }
}

// ---------- where things go ----------

#[derive(Debug, Clone)]
pub struct Layout {
    pub updates: PathBuf,
    pub versions: PathBuf,
}

impl Default for Layout {
    fn default() -> Self {
        let h = crate::config::app_home();
        Layout {
            updates: h.join("updates"),
            versions: h.join("versions"),
        }
    }
}

/// Where releases come from and what a download must pass.
#[derive(Debug, Clone)]
pub struct Source {
    pub api: String,
    pub repo: String,
    /// Tests: a mock server on http://127.0.0.1 is allowed.
    pub local_ok: bool,
    /// Check the Developer ID signature of macOS downloads.
    pub codesign: bool,
    /// A release without SHA256SUMS.minisig is refused.
    pub require_sig: bool,
    pub pubkey: String,
    /// Tests: take this release file instead of the platform's.
    pub asset: Option<String>,
}

impl Source {
    pub fn github(require_sig: bool) -> Source {
        Source {
            api: crate::config::env_var("UPDATE_API")
                .filter(|_| cfg!(test))
                .unwrap_or_else(|| API.to_string()),
            repo: REPO.to_string(),
            local_ok: false,
            codesign: cfg!(target_os = "macos"),
            require_sig,
            pubkey: UPDATE_PUBKEY.to_string(),
            asset: None,
        }
    }
}

/// HTTPS to GitHub only (or the test server).
pub fn url_allowed(url: &str, local_ok: bool) -> bool {
    if local_ok && url.starts_with("http://127.0.0.1:") {
        return true;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.contains('@') {
        return false;
    }
    let host = host.split(':').next().unwrap_or("").to_ascii_lowercase();
    HOSTS.contains(&host.as_str())
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .user_agent(format!("godterm/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

type Resp = ureq::http::Response<ureq::Body>;

/// GET, following redirects by hand so every hop is checked.
fn get(src: &Source, url: &str, headers: &[(&str, String)], timeout: Duration) -> Result<Resp> {
    let a = agent(timeout);
    let mut url = url.to_string();
    for _ in 0..6 {
        if !url_allowed(&url, src.local_ok) {
            bail!("refused to download from {url} (only https from GitHub)");
        }
        let mut req = a.get(&url);
        for (k, v) in headers {
            req = req.header(*k, v.as_str());
        }
        let resp = req.call().map_err(|e| anyhow!("{url}: {e}"))?;
        let code = resp.status().as_u16();
        if matches!(code, 301 | 302 | 303 | 307 | 308) {
            let loc = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect without a location"))?;
            url = if loc.starts_with('/') {
                let base: String = url.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
                format!("{base}{loc}")
            } else {
                loc.to_string()
            };
            continue;
        }
        return Ok(resp);
    }
    bail!("too many redirects")
}

fn header(r: &Resp, k: &str) -> Option<String> {
    r.headers()
        .get(k)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

// ---------- checking ----------

/// The check cache.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cache {
    pub etag: Option<String>,
    pub release: Option<Release>,
    /// Unix seconds.
    pub checked_at: Option<i64>,
    pub backoff_until: Option<i64>,
    pub skipped: Option<String>,
    pub last_error: Option<String>,
}

impl Cache {
    pub fn path(l: &Layout) -> PathBuf {
        l.updates.join("state.json")
    }

    pub fn load(l: &Layout) -> Cache {
        std::fs::read_to_string(Cache::path(l))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, l: &Layout) {
        let _ = std::fs::create_dir_all(&l.updates);
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = crate::config::write_private(&Cache::path(l), s);
        }
    }
}

fn now_s() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The latest release on `channel` ("stable", or "prerelease" to include
/// pre-releases). A 304 answers from the cache; 403 or 429 backs off.
pub fn check(src: &Source, cache: &mut Cache, channel: &str) -> Result<Option<Release>> {
    if let Some(t) = cache.backoff_until.filter(|t| *t > now_s()) {
        bail!(
            "GitHub asked to wait (rate limit); next check after {}",
            chrono::DateTime::from_timestamp(t, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
                .unwrap_or_default()
        );
    }
    let pre = channel == "prerelease";
    let url = if pre {
        format!("{}/repos/{}/releases?per_page=10", src.api, src.repo)
    } else {
        format!("{}/repos/{}/releases/latest", src.api, src.repo)
    };
    let mut h = vec![
        ("Accept", "application/vnd.github+json".to_string()),
        ("X-GitHub-Api-Version", "2022-11-28".to_string()),
    ];
    if let (Some(e), Some(_)) = (&cache.etag, &cache.release) {
        h.push(("If-None-Match", e.clone()));
    }
    let mut r = get(src, &url, &h, Duration::from_secs(20))?;
    cache.checked_at = Some(now_s());
    let code = r.status().as_u16();
    match code {
        304 => {
            crate::log::info("update: not modified (cached release)");
            Ok(cache.release.clone())
        }
        200 => {
            let etag = header(&r, "etag");
            let body = r
                .body_mut()
                .with_config()
                .limit(4 * 1024 * 1024)
                .read_to_string()?;
            let v: Value = serde_json::from_str(&body).context("the release reply is not JSON")?;
            let rel = if pre {
                v.as_array()
                    .and_then(|a| a.iter().find_map(Release::from_json))
            } else {
                Release::from_json(&v)
            };
            cache.etag = etag;
            cache.release = rel.clone();
            cache.last_error = None;
            Ok(rel)
        }
        404 => Ok(None),
        403 | 429 => {
            let wait = header(&r, "retry-after")
                .and_then(|s| s.parse::<i64>().ok())
                .map(|s| now_s() + s)
                .or_else(|| {
                    header(&r, "x-ratelimit-reset")
                        .and_then(|s| s.parse::<i64>().ok())
                        .filter(|t| *t > now_s())
                })
                .unwrap_or(now_s() + 3600);
            cache.backoff_until = Some(wait);
            crate::log::info(&format!("update: GitHub said {code}; backing off"));
            bail!("GitHub rate limited the check ({code}); trying again later")
        }
        c => bail!("GitHub answered {c}"),
    }
}

// ---------- install methods ----------

#[derive(Debug, Clone, PartialEq)]
pub enum Method {
    /// A link into ~/.godterm/versions (an earlier update made it).
    Managed {
        link: PathBuf,
    },
    /// A plain binary GodTerm may replace (install.sh's ~/.local/bin, a zip).
    Standalone {
        path: PathBuf,
    },
    /// The notarized GodTerm.app from the DMG: the whole bundle is swapped.
    AppBundle {
        app: PathBuf,
    },
    AppImage {
        path: PathBuf,
    },
    /// The per-user Windows installer: the new installer runs on restart.
    WindowsInstaller {
        exe: PathBuf,
    },
    Homebrew,
    /// /usr from a .deb or .rpm.
    System {
        command: String,
    },
    /// A cargo build: never replaced.
    Dev {
        path: PathBuf,
    },
    /// Somewhere it cannot write.
    ReadOnly {
        path: PathBuf,
    },
}

impl Method {
    /// What the user should run instead, when GodTerm must not update itself.
    pub fn hint(&self) -> Option<String> {
        match self {
            Method::Homebrew => Some("brew upgrade godterm".into()),
            Method::System { command } => Some(command.clone()),
            Method::Dev { .. } => Some("a development build: git pull and cargo build".into()),
            Method::ReadOnly { path } => Some(format!(
                "{} is not writable: run sudo godterm update",
                path.display()
            )),
            _ => None,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Method::Managed { link } => format!("{} (self update)", link.display()),
            Method::Standalone { path } => format!("{} (self update)", path.display()),
            Method::AppBundle { app } => format!("{} (self update)", app.display()),
            Method::AppImage { path } => format!("AppImage {} (self update)", path.display()),
            Method::WindowsInstaller { exe } => format!("{} (installer)", exe.display()),
            Method::Homebrew => "Homebrew".into(),
            Method::System { .. } => "system package".into(),
            Method::Dev { path } => format!("development build {}", path.display()),
            Method::ReadOnly { path } => format!("{} (not writable)", path.display()),
        }
    }

    /// The release file this install takes.
    pub fn asset(&self, os: &str, arch: &str) -> Option<String> {
        let arch = match arch {
            "aarch64" | "arm64" => "aarch64",
            _ => "x86_64",
        };
        match self {
            Method::AppBundle { .. } => Some("GodTerm-macos-universal.dmg".into()),
            Method::AppImage { .. } => Some(format!("GodTerm-{arch}.AppImage")),
            Method::WindowsInstaller { .. } => Some("GodTerm-windows-x64-setup.exe".into()),
            Method::Managed { .. } | Method::Standalone { .. } => Some(match os {
                "macos" => "godterm-macos-universal.zip".into(),
                "windows" => format!("godterm-{arch}-pc-windows-msvc.zip"),
                _ => format!("godterm-{arch}-unknown-linux-gnu.tar.gz"),
            }),
            _ => None,
        }
    }
}

/// How this copy was installed. `exe` is the running binary as invoked
/// (`invoked`, a link maybe) and resolved (`real`).
pub fn detect(invoked: &Path, real: &Path, appimage: Option<&Path>, l: &Layout) -> Method {
    let r = real.to_string_lossy().replace('\\', "/");
    if let Some(a) = appimage {
        return Method::AppImage {
            path: a.to_path_buf(),
        };
    }
    if r.contains("/Cellar/") || r.starts_with("/opt/homebrew/") || r.contains("/linuxbrew/") {
        return Method::Homebrew;
    }
    if ["/target/release/", "/target/debug/", "/target/dist/"]
        .iter()
        .any(|t| r.contains(t))
    {
        return Method::Dev {
            path: real.to_path_buf(),
        };
    }
    if real.starts_with(&l.versions) {
        let link = if invoked != real && invoked.is_symlink() {
            invoked.to_path_buf()
        } else {
            Installed::load(l)
                .target
                .unwrap_or_else(crate::install::cli_link)
        };
        return Method::Managed { link };
    }
    if let Some(i) = r.find(".app/Contents/MacOS/") {
        return Method::AppBundle {
            app: PathBuf::from(&r[..i + 4]),
        };
    }
    if cfg!(target_os = "linux") && r.starts_with("/usr/") && !r.starts_with("/usr/local/") {
        let command = if Path::new("/usr/bin/apt").exists() || Path::new("/usr/bin/dpkg").exists() {
            "sudo apt update && sudo apt install --only-upgrade godterm (or the new .deb)"
        } else if Path::new("/usr/bin/dnf").exists() {
            "sudo dnf upgrade godterm (or the new .rpm)"
        } else {
            "update the godterm package with your package manager"
        };
        return Method::System {
            command: command.into(),
        };
    }
    let lower = r.to_ascii_lowercase();
    if lower.contains("/appdata/local/programs/godterm/") {
        return Method::WindowsInstaller {
            exe: real.to_path_buf(),
        };
    }
    let target = if invoked.is_symlink() { invoked } else { real };
    match target.parent() {
        Some(d) if crate::platform::writable(d) => Method::Standalone {
            path: target.to_path_buf(),
        },
        _ => Method::ReadOnly {
            path: target.to_path_buf(),
        },
    }
}

/// This process's install method.
pub fn detect_self(l: &Layout) -> Method {
    let invoked = std::env::current_exe().unwrap_or_default();
    let real = std::fs::canonicalize(&invoked).unwrap_or_else(|_| invoked.clone());
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    detect(&invoked, &real, appimage.as_deref(), l)
}

pub fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

// ---------- download and verify ----------

pub fn sha256_file(p: &Path) -> Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// "hash  name" lines (also "hash *name").
pub fn parse_sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (h, n) = l.trim().split_once(char::is_whitespace)?;
            let n = n.trim().trim_start_matches('*');
            (h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()) && !n.is_empty())
                .then(|| (h.to_ascii_lowercase(), n.to_string()))
        })
        .collect()
}

/// SHA256SUMS against its minisign signature.
pub fn verify_sig(sums: &[u8], sig: &str, pubkey: &str) -> Result<()> {
    let pk = minisign_verify::PublicKey::from_base64(pubkey.trim())
        .map_err(|e| anyhow!("bad public key: {e}"))?;
    let s =
        minisign_verify::Signature::decode(sig).map_err(|e| anyhow!("bad signature file: {e}"))?;
    pk.verify(sums, &s, false)
        .map_err(|e| anyhow!("signature check failed: {e}"))
}

fn small(src: &Source, url: &str) -> Result<Vec<u8>> {
    let mut r = get(src, url, &[], Duration::from_secs(30))?;
    if r.status().as_u16() != 200 {
        bail!("{url}: {}", r.status());
    }
    Ok(r.body_mut().with_config().limit(1 << 20).read_to_vec()?)
}

/// Download `url` to `dest`, resuming a `.part` left by an interrupted try.
pub fn download(src: &Source, url: &str, dest: &Path, progress: &dyn Fn(u64, u64)) -> Result<()> {
    let part = PathBuf::from(format!("{}.part", dest.display()));
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d)?;
    }
    for attempt in 0..2 {
        let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let h = if have > 0 {
            vec![("Range", format!("bytes={have}-"))]
        } else {
            vec![]
        };
        let mut r = get(src, url, &h, Duration::from_secs(1800))?;
        let code = r.status().as_u16();
        let (mut f, start) = match code {
            206 if have > 0 => (std::fs::OpenOptions::new().append(true).open(&part)?, have),
            200 => (std::fs::File::create(&part)?, 0),
            416 if attempt == 0 => {
                let _ = std::fs::remove_file(&part);
                continue;
            }
            c => bail!("download answered {c}"),
        };
        if start > 0 {
            crate::log::info(&format!("update: resuming at {start} bytes"));
        }
        let total = start
            + header(&r, "content-length")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
        let mut body = r.body_mut().with_config().limit(1 << 31).reader();
        let mut buf = vec![0u8; 1 << 16];
        let mut got = start;
        loop {
            let n = body.read(&mut buf).context("the download was cut off")?;
            if n == 0 {
                break;
            }
            f.write_all(&buf[..n])?;
            got += n as u64;
            progress(got, total);
        }
        f.flush()?;
        drop(f);
        if total > 0 && got < total {
            bail!("the download was cut off at {got} of {total} bytes");
        }
        std::fs::rename(&part, dest)?;
        return Ok(());
    }
    bail!("could not download {url}")
}

/// A verified download, ready to install.
#[derive(Debug, Clone, PartialEq)]
pub struct Staged {
    pub version: String,
    pub dir: PathBuf,
    pub payload: Payload,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Binary {
        bin: PathBuf,
        speech: Option<PathBuf>,
    },
    AppImage(PathBuf),
    Dmg(PathBuf),
    Installer(PathBuf),
}

fn run(cmd: &mut std::process::Command, secs: u64) -> Result<std::process::Output> {
    crate::procs::output_timeout(cmd, Duration::from_secs(secs)).map_err(|e| anyhow!(e))
}

/// The Developer ID signature of a macOS binary or bundle: valid, by our
/// team, and (for a bundle) accepted by Gatekeeper.
pub fn verify_codesign(p: &Path, bundle: bool) -> Result<()> {
    let mut c = std::process::Command::new("codesign");
    c.args(["--verify", "--strict"]);
    if bundle {
        c.arg("--deep");
    }
    let o = run(c.arg(p), 120)?;
    if !o.status.success() {
        bail!(
            "codesign rejected {}: {}",
            p.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    let o = run(
        std::process::Command::new("codesign")
            .args(["-d", "--verbose=2"])
            .arg(p),
        60,
    )?;
    let info = String::from_utf8_lossy(&o.stderr).to_string();
    if !info.contains(&format!("TeamIdentifier={TEAM_ID}"))
        || !info.contains("Authority=Developer ID Application")
    {
        bail!("{} is not signed by GodTerm's Developer ID", p.display());
    }
    if bundle {
        let o = run(
            std::process::Command::new("spctl")
                .args(["--assess", "--type", "execute"])
                .arg(p),
            120,
        )?;
        if !o.status.success() {
            bail!(
                "Gatekeeper rejected {}: {}",
                p.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
    }
    Ok(())
}

fn find_file(dir: &Path, names: &[&str], depth: u32) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut dirs = vec![];
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        } else if p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| names.contains(&n))
        {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    dirs.iter().find_map(|d| find_file(d, names, depth - 1))
}

fn extract(archive: &Path, into: &Path) -> Result<()> {
    if into.exists() {
        std::fs::remove_dir_all(into)?;
    }
    std::fs::create_dir_all(into)?;
    let name = archive.to_string_lossy().to_string();
    let mut c = if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let mut c = std::process::Command::new("tar");
        c.arg("-xzf").arg(archive).arg("-C").arg(into);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("ditto");
        c.args(["-x", "-k"]).arg(archive).arg(into);
        c
    } else {
        // bsdtar (Windows 10+, macOS) reads zips; Linux has unzip.
        let mut c = std::process::Command::new(if cfg!(windows) { "tar" } else { "unzip" });
        if cfg!(windows) {
            c.arg("-xf").arg(archive).arg("-C").arg(into);
        } else {
            c.arg("-q").arg(archive).arg("-d").arg(into);
        }
        c
    };
    let o = run(&mut c, 300)?;
    if !o.status.success() {
        bail!(
            "could not unpack {}: {}",
            archive.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(())
}

/// Download and verify `rel` for `method` into `l.updates/<v>`. Nothing is
/// installed; a failed check leaves nothing behind.
pub fn stage(
    src: &Source,
    rel: &Release,
    method: &Method,
    l: &Layout,
    progress: &dyn Fn(u64, u64),
) -> Result<Staged> {
    let v = rel.version().to_string();
    let alias = src
        .asset
        .clone()
        .or_else(|| method.asset(os_name(), std::env::consts::ARCH))
        .ok_or_else(|| anyhow!("this install updates through {}", method.describe()))?;
    let asset = rel
        .pick(&alias)
        .ok_or_else(|| anyhow!("release {} has no {alias}", rel.tag))?
        .clone();
    let sums_url = &rel
        .asset("SHA256SUMS")
        .ok_or_else(|| anyhow!("release {} has no SHA256SUMS", rel.tag))?
        .url;
    let sums = small(src, sums_url)?;
    match rel.asset("SHA256SUMS.minisig") {
        Some(a) => {
            let sig = String::from_utf8_lossy(&small(src, &a.url)?).to_string();
            verify_sig(&sums, &sig, &src.pubkey).context("refused")?;
            crate::log::info(&format!("update: SHA256SUMS of {v} signature ok"));
        }
        None if src.require_sig => bail!(
            "release {} is not signed; refusing to update. Set update.require_signature = false to override",
            rel.tag
        ),
        None => crate::log::info(&format!(
            "update: {v} has no signature (update.require_signature is off)"
        )),
    }
    let want = parse_sums(&String::from_utf8_lossy(&sums))
        .into_iter()
        .find(|(_, n)| *n == asset.name)
        .map(|(h, _)| h)
        .ok_or_else(|| anyhow!("SHA256SUMS does not list {}", asset.name))?;
    let dir = l.updates.join(&v);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(&asset.name);
    if !file.exists() || sha256_file(&file).ok().as_deref() != Some(want.as_str()) {
        let _ = std::fs::remove_file(&file);
        crate::log::info(&format!("update: downloading {}", asset.url));
        download(src, &asset.url, &file, progress)?;
    }
    let got = sha256_file(&file)?;
    if got != want {
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(format!("{}.part", file.display()));
        bail!(
            "refused: checksum mismatch for {} (expected {want}, got {got})",
            asset.name
        );
    }
    crate::log::info(&format!("update: {} sha256 ok", asset.name));
    let n = asset.name.as_str();
    let payload = if n.ends_with(".AppImage") {
        chmod_x(&file)?;
        Payload::AppImage(file)
    } else if n.ends_with(".dmg") {
        Payload::Dmg(file)
    } else if n.ends_with(".exe") {
        Payload::Installer(file)
    } else {
        let x = dir.join("x");
        extract(&file, &x)?;
        let bin = find_file(&x, &["godterm", "godterm.exe"], 3)
            .ok_or_else(|| anyhow!("{} has no godterm binary", asset.name))?;
        chmod_x(&bin)?;
        let speech = find_file(&x, &["godterm-speech"], 3);
        if let Some(s) = &speech {
            chmod_x(s)?;
        }
        if src.codesign && cfg!(target_os = "macos") {
            if let Err(e) = verify_codesign(&bin, false) {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e.context("refused"));
            }
        }
        Payload::Binary { bin, speech }
    };
    Ok(Staged {
        version: v,
        dir,
        payload,
    })
}

fn chmod_x(p: &Path) -> Result<()> {
    use crate::platform::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

// ---------- installing ----------

/// What is active and what came before (for rollback).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Installed {
    /// The path that was replaced (the link, the AppImage, the bundle).
    pub target: Option<PathBuf>,
    pub active: Option<String>,
    pub previous: Option<String>,
    /// The previous version's copy.
    pub previous_path: Option<PathBuf>,
    /// The new version's copy.
    pub active_path: Option<PathBuf>,
    pub kind: String,
}

impl Installed {
    pub fn path(l: &Layout) -> PathBuf {
        l.versions.join("state.json")
    }

    pub fn load(l: &Layout) -> Installed {
        std::fs::read_to_string(Installed::path(l))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, l: &Layout) -> Result<()> {
        std::fs::create_dir_all(&l.versions)?;
        let tmp = l.versions.join(".state.json.new");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, Installed::path(l))?;
        Ok(())
    }
}

/// What runs after the install: the program, and (Windows installer) a
/// command to run first.
#[derive(Debug, Clone, PartialEq)]
pub struct Next {
    pub program: PathBuf,
    pub before: Option<(PathBuf, Vec<String>)>,
}

/// A file copied in place atomically: written beside, then renamed.
fn place(from: &Path, to: &Path) -> Result<()> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = PathBuf::from(format!("{}.new", to.display()));
    std::fs::copy(from, &tmp).with_context(|| format!("copying to {}", tmp.display()))?;
    chmod_x(&tmp)?;
    std::fs::rename(&tmp, to)?;
    Ok(())
}

/// Point `link` at `target` in one rename (no moment without a program).
fn swap_link(link: &Path, target: &Path) -> Result<()> {
    let tmp = link.with_file_name(format!(
        ".{}.link-new",
        link.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("godterm")
    ));
    let _ = std::fs::remove_file(&tmp);
    crate::platform::symlink(target, &tmp)?;
    std::fs::rename(&tmp, link).with_context(|| format!("replacing {}", link.display()))?;
    Ok(())
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "godterm.exe"
    } else {
        "godterm"
    }
}

/// Install `staged` for `method`. A downgrade (or the same version) is
/// refused unless `force`. Verified before, so nothing half done is left:
/// each step is a copy beside the target and one rename.
pub fn install(
    staged: &Staged,
    method: &Method,
    cur: &Version,
    force: bool,
    l: &Layout,
) -> Result<Next> {
    let new = Version::parse(&staged.version).ok_or_else(|| anyhow!("bad version"))?;
    if new < *cur && !force {
        bail!("refused: {new} is older than {cur} (use --force to downgrade)");
    }
    if new == *cur && !force {
        bail!("already on {cur}");
    }
    let mut rec = Installed {
        active: Some(new.to_string()),
        previous: Some(cur.to_string()),
        ..Default::default()
    };
    let next = match (method, &staged.payload) {
        (
            Method::Standalone { path } | Method::Managed { link: path },
            Payload::Binary { bin, speech },
        ) => {
            let vdir = l.versions.join(new.to_string());
            let nbin = vdir.join(exe_name());
            place(bin, &nbin)?;
            if let Some(s) = speech {
                place(s, &vdir.join("godterm-speech"))?;
            }
            // The running copy, kept for rollback.
            let old_dir = l.versions.join(cur.to_string());
            let old_bin = old_dir.join(exe_name());
            let is_link = path.is_symlink();
            if !old_bin.exists() {
                let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
                place(&real, &old_bin)?;
                let sp = path.with_file_name("godterm-speech");
                if sp.exists() {
                    let r = std::fs::canonicalize(&sp).unwrap_or(sp);
                    let _ = place(&r, &old_dir.join("godterm-speech"));
                }
            }
            if cfg!(windows) {
                // A running exe cannot be overwritten, but it can be renamed.
                let old = path.with_extension("exe.old");
                let _ = std::fs::remove_file(&old);
                if path.exists() {
                    std::fs::rename(path, &old)?;
                }
                if let Err(e) = place(&nbin, path) {
                    let _ = std::fs::rename(&old, path);
                    return Err(e);
                }
            } else {
                swap_link(path, &nbin)?;
                let sp = path.with_file_name("godterm-speech");
                if (sp.exists() || sp.is_symlink()) && vdir.join("godterm-speech").exists() {
                    swap_link(&sp, &vdir.join("godterm-speech"))?;
                }
            }
            crate::log::info(&format!(
                "update: {} now runs {new}{}",
                path.display(),
                if is_link {
                    ""
                } else {
                    " (now a link into ~/.godterm/versions)"
                }
            ));
            rec.kind = "binary".into();
            rec.target = Some(path.clone());
            rec.previous_path = Some(old_bin);
            rec.active_path = Some(nbin);
            Next {
                program: path.clone(),
                before: None,
            }
        }
        (Method::AppImage { path }, Payload::AppImage(img)) => {
            let old = l.versions.join(cur.to_string()).join("GodTerm.AppImage");
            if !old.exists() {
                place(path, &old)?;
            }
            let keep = l.versions.join(new.to_string()).join("GodTerm.AppImage");
            place(img, &keep)?;
            place(img, path)?;
            rec.kind = "appimage".into();
            rec.target = Some(path.clone());
            rec.previous_path = Some(old);
            rec.active_path = Some(keep);
            Next {
                program: path.clone(),
                before: None,
            }
        }
        (Method::AppBundle { app }, Payload::Dmg(dmg)) => {
            let staged_app = bundle_from_dmg(dmg, app, &new, &staged.dir)?;
            let old = l.versions.join(cur.to_string()).join("GodTerm.app");
            swap_bundle(app, &staged_app, &old)?;
            rec.kind = "bundle".into();
            rec.target = Some(app.clone());
            rec.previous_path = Some(old);
            Next {
                program: app.join("Contents/MacOS/GodTerm"),
                before: None,
            }
        }
        (Method::WindowsInstaller { exe }, Payload::Installer(setup)) => {
            // The installer writes a fresh exe; this one moves aside first.
            let old = exe.with_extension("exe.old");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(exe, &old)?;
            rec.kind = "installer".into();
            rec.target = Some(exe.clone());
            rec.previous_path = Some(old);
            Next {
                program: exe.clone(),
                before: Some((
                    setup.clone(),
                    ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                )),
            }
        }
        (m, _) => bail!(
            "this install updates through {}{}",
            m.describe(),
            m.hint().map(|h| format!(": {h}")).unwrap_or_default()
        ),
    };
    rec.save(l)?;
    prune(l, &[new.to_string(), cur.to_string()]);
    let _ = std::fs::remove_dir_all(&staged.dir);
    crate::log::info(&format!("update: installed {new} (previous {cur} kept)"));
    Ok(next)
}

/// Copy GodTerm.app out of the DMG next to `app` and check its signature.
fn bundle_from_dmg(dmg: &Path, app: &Path, v: &Version, work: &Path) -> Result<PathBuf> {
    let mnt = work.join("mnt");
    std::fs::create_dir_all(&mnt)?;
    let o = run(
        std::process::Command::new("hdiutil")
            .args([
                "attach",
                "-nobrowse",
                "-readonly",
                "-noautoopen",
                "-mountpoint",
            ])
            .arg(&mnt)
            .arg(dmg),
        300,
    )?;
    if !o.status.success() {
        bail!(
            "could not open the DMG: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    let parent = app.parent().ok_or_else(|| anyhow!("no folder"))?;
    let out = parent.join(format!(".GodTerm-{v}.app"));
    let _ = std::fs::remove_dir_all(&out);
    let copied = run(
        std::process::Command::new("ditto")
            .arg(mnt.join("GodTerm.app"))
            .arg(&out),
        600,
    );
    let _ = run(
        std::process::Command::new("hdiutil")
            .args(["detach", "-quiet"])
            .arg(&mnt),
        120,
    );
    let o = copied?;
    if !o.status.success() {
        let _ = std::fs::remove_dir_all(&out);
        bail!(
            "could not copy the app: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    if let Err(e) = verify_codesign(&out, true) {
        let _ = std::fs::remove_dir_all(&out);
        return Err(e.context("refused"));
    }
    Ok(out)
}

/// Whole bundles only (a signed bundle is never edited in place): the old
/// one moves to `keep`, the new one takes its name.
fn swap_bundle(app: &Path, new_app: &Path, keep: &Path) -> Result<()> {
    if let Some(d) = keep.parent() {
        std::fs::create_dir_all(d)?;
    }
    let _ = std::fs::remove_dir_all(keep);
    let aside = app.with_file_name(".GodTerm.app.old");
    let _ = std::fs::remove_dir_all(&aside);
    std::fs::rename(app, &aside)?;
    if let Err(e) = std::fs::rename(new_app, app) {
        let _ = std::fs::rename(&aside, app);
        return Err(e.into());
    }
    // Across volumes the old copy stays beside the app.
    if std::fs::rename(&aside, keep).is_err() {
        crate::log::info(&format!("update: previous app kept at {}", aside.display()));
    }
    Ok(())
}

/// Back to the version before the last update.
pub fn rollback(l: &Layout) -> Result<String> {
    let mut rec = Installed::load(l);
    let (Some(target), Some(prev), Some(prev_path)) = (
        rec.target.clone(),
        rec.previous.clone(),
        rec.previous_path.clone(),
    ) else {
        bail!("nothing to roll back to (no update was installed here)");
    };
    if !prev_path.exists() {
        bail!("the previous version is gone ({})", prev_path.display());
    }
    match rec.kind.as_str() {
        "binary" if !cfg!(windows) => {
            swap_link(&target, &prev_path)?;
            let sp = target.with_file_name("godterm-speech");
            let psp = prev_path.with_file_name("godterm-speech");
            if sp.is_symlink() && psp.exists() {
                swap_link(&sp, &psp)?;
            }
        }
        "binary" | "appimage" => {
            if cfg!(windows) {
                let old = target.with_extension("exe.old");
                let _ = std::fs::remove_file(&old);
                let _ = std::fs::rename(&target, &old);
            }
            place(&prev_path, &target)?;
        }
        "bundle" => {
            let aside = target.with_file_name(".GodTerm.app.new-rollback");
            let _ = std::fs::remove_dir_all(&aside);
            std::fs::rename(&prev_path, &aside)?;
            let newer = l
                .versions
                .join(rec.active.clone().unwrap_or_default())
                .join("GodTerm.app");
            swap_bundle(&target, &aside, &newer)?;
            rec.previous_path = Some(newer);
        }
        k => bail!("cannot roll back a {k} install"),
    }
    let was = rec.active.take();
    rec.active = Some(prev.clone());
    rec.previous = was;
    if rec.kind != "bundle" {
        let ap = rec.active_path.take();
        rec.active_path = Some(prev_path);
        rec.previous_path = ap;
    }
    rec.save(l)?;
    crate::log::info(&format!("update: rolled back to {prev}"));
    Ok(prev)
}

/// Drop installed versions other than `keep` (and the running one).
fn prune(l: &Layout, keep: &[String]) {
    let running = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok());
    let Ok(rd) = std::fs::read_dir(&l.versions) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if !p.is_dir() || keep.contains(&name) || Version::parse(&name).is_none() {
            continue;
        }
        if running.as_ref().is_some_and(|r| r.starts_with(&p)) {
            continue;
        }
        let _ = std::fs::remove_dir_all(&p);
    }
}

// ---------- restart ----------

static RESTART: std::sync::Mutex<Option<Next>> = std::sync::Mutex::new(None);

/// Ask main() to run `next` once the TUI has shut down.
pub fn request_restart(next: Next) {
    *RESTART.lock().unwrap_or_else(|e| e.into_inner()) = Some(next);
}

#[cfg(test)]
pub fn take_restart() -> Option<Next> {
    RESTART.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The environment mark that makes the next start restore every tab at
/// once (`--resume`), as after a restart to update.
pub const EAGER_ENV: &str = "GODTERM_RESTORE_EAGER";

/// After the TUI is down: become the new version, same arguments.
pub fn relaunch_if_requested() {
    let Some(next) = RESTART.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    if let Some((cmd, args)) = &next.before {
        crate::log::info(&format!("update: running {}", cmd.display()));
        let _ = std::process::Command::new(cmd).args(args).status();
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    crate::log::info(&format!(
        "update: restarting into {}",
        next.program.display()
    ));
    std::env::set_var(EAGER_ENV, "1");
    let e = crate::platform::exec_replace(&next.program, &args);
    eprintln!(
        "godterm: could not start {}: {e}; run it yourself",
        next.program.display()
    );
}

// ---------- the command ----------

/// `godterm update [--check] [--rollback] [--force]`.
pub fn command(args: &[String]) -> Result<()> {
    let has = |f: &str| args.iter().any(|a| a == f);
    let l = Layout::default();
    if has("--rollback") {
        let v = rollback(&l)?;
        println!("Rolled back to {v}. Restart GodTerm to use it.");
        return Ok(());
    }
    let cfg = crate::config::Config::load_or_init().unwrap_or_default();
    let src = Source::github(cfg.update.require_signature);
    let method = detect_self(&l);
    let cur = Version::current();
    let mut cache = Cache::load(&l);
    let rel = check(&src, &mut cache, &cfg.update.channel);
    cache.save(&l);
    let rel = rel?;
    println!("installed   {cur} ({})", method.describe());
    let Some(rel) = rel else {
        println!("latest      none published");
        return Ok(());
    };
    let latest = rel.version();
    println!("latest      {latest} {}", rel.page);
    if latest <= cur && !has("--force") {
        println!("Up to date.");
        return Ok(());
    }
    if has("--check") {
        match method.hint() {
            Some(h) => println!("Update with: {h}"),
            None => println!("Run godterm update to install it."),
        }
        return Ok(());
    }
    if let Some(h) = method.hint() {
        println!("GodTerm does not update this install itself. Run: {h}");
        return Ok(());
    }
    let shown = std::cell::Cell::new(0u64);
    let staged = stage(&src, &rel, &method, &l, &|got, total| {
        let pct = (got * 100).checked_div(total).unwrap_or(0);
        if pct >= shown.get() + 10 {
            shown.set(pct);
            eprint!("\rdownloading {pct}%");
        }
    })?;
    eprintln!();
    println!(
        "verified    {} (sha256{})",
        staged.version,
        if rel.asset("SHA256SUMS.minisig").is_some() {
            ", signature"
        } else {
            ""
        }
    );
    let next = install(&staged, &method, &cur, has("--force"), &l)?;
    if let Some((cmd, a)) = &next.before {
        let st = std::process::Command::new(cmd).args(a).status()?;
        if !st.success() {
            bail!("the installer failed ({st})");
        }
    }
    println!(
        "Updated to {latest}: {}. Restart GodTerm to use it (Ctrl-a N in a running GodTerm); godterm update --rollback goes back.",
        next.program.display()
    );
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    pub const TEST_PUB: &str = include_str!("../tests/fixtures/update/test.pub");
    pub const SUMS: &[u8] = include_bytes!("../tests/fixtures/update/SHA256SUMS");
    pub const SIG: &str = include_str!("../tests/fixtures/update/SHA256SUMS.minisig");
    pub const ARCHIVE: &[u8] = include_bytes!("../tests/fixtures/update/godterm-test.tar.gz");

    fn test_pubkey() -> String {
        TEST_PUB.lines().nth(1).unwrap().to_string()
    }

    /// A release server on 127.0.0.1: routes to bodies; `cut` closes the
    /// first full download of the archive halfway.
    pub struct Mock {
        pub base: String,
        pub hits: Arc<Mutex<Vec<String>>>,
    }

    pub fn mock_routes(f: impl FnOnce(&str) -> Vec<(String, Vec<u8>)>, cut: bool) -> Mock {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let routes = f(&base);
        let hits = Arc::new(Mutex::new(vec![]));
        let h2 = hits.clone();
        let cut_once = Arc::new(Mutex::new(cut));
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { return };
                let mut r = std::io::BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split(' ').nth(1).unwrap_or("/").to_string();
                let mut range = None;
                let mut inm = None;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    let lower = h.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                    if let Some(v) = lower.strip_prefix("if-none-match: ") {
                        inm = Some(v.trim().to_string());
                    }
                }
                h2.lock().unwrap().push(format!(
                    "{path}{}",
                    range.map(|r| format!(" range={r}")).unwrap_or_default()
                ));
                if path.contains("/limited/") {
                    let _ = s.write_all(b"HTTP/1.1 429 Too Many\r\nRetry-After: 120\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
                let Some((_, body)) = routes.iter().find(|(p, _)| *p == path) else {
                    let _ = s.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                };
                if path.ends_with("/latest") && inm.as_deref() == Some("\"e1\"") {
                    let _ = s.write_all(b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
                let start = range.unwrap_or(0).min(body.len());
                let part = &body[start..];
                let head = if range.is_some() {
                    format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{}/{}\r\nConnection: close\r\n\r\n", part.len(), body.len().saturating_sub(1), body.len())
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"e1\"\r\nConnection: close\r\n\r\n", part.len())
                };
                let _ = s.write_all(head.as_bytes());
                let mut c = cut_once.lock().unwrap();
                if *c && path.ends_with(".tar.gz") && range.is_none() {
                    *c = false;
                    let _ = s.write_all(&part[..part.len() / 2]);
                    let _ = s.flush();
                    let _ = s.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                let _ = s.write_all(part);
            }
        });
        Mock { base, hits }
    }

    pub fn release_json(base: &str, tag: &str, signed: bool) -> Vec<u8> {
        let mut assets = vec![
            serde_json::json!({"name": "godterm-test.tar.gz", "browser_download_url": format!("{base}/dl/godterm-test.tar.gz"), "size": ARCHIVE.len()}),
            serde_json::json!({"name": "SHA256SUMS", "browser_download_url": format!("{base}/dl/SHA256SUMS"), "size": SUMS.len()}),
        ];
        if signed {
            assets.push(serde_json::json!({"name": "SHA256SUMS.minisig", "browser_download_url": format!("{base}/dl/SHA256SUMS.minisig"), "size": SIG.len()}));
        }
        serde_json::to_vec(&serde_json::json!({
            "tag_name": tag, "html_url": format!("{base}/rel/{tag}"), "prerelease": false, "draft": false,
            "body": "## What's new\n- **Faster** restarts\n- Fixes", "assets": assets,
        }))
        .unwrap()
    }

    pub fn test_source(base: &str) -> Source {
        Source {
            api: base.to_string(),
            repo: REPO.to_string(),
            local_ok: true,
            codesign: false,
            require_sig: false,
            pubkey: test_pubkey(),
            asset: Some("godterm-test.tar.gz".into()),
        }
    }

    /// A tar.gz release in the layout tests use (the platform's real
    /// archive name differs; the method maps to it).
    pub fn test_method(path: PathBuf) -> Method {
        Method::Standalone { path }
    }

    pub fn routes(base: &str, tag: &str, signed: bool, sums: &[u8]) -> Vec<(String, Vec<u8>)> {
        vec![
            (
                format!("/repos/{REPO}/releases/latest"),
                release_json(base, tag, signed),
            ),
            ("/dl/godterm-test.tar.gz".into(), ARCHIVE.to_vec()),
            ("/dl/SHA256SUMS".into(), sums.to_vec()),
            ("/dl/SHA256SUMS.minisig".into(), SIG.as_bytes().to_vec()),
        ]
    }

    fn sums_for(alias: &str) -> Vec<u8> {
        String::from_utf8_lossy(SUMS)
            .replace("godterm-test.tar.gz", alias)
            .into_bytes()
    }

    fn tmp(tag: &str) -> (PathBuf, Layout) {
        let d = std::env::temp_dir().join(format!("godterm-upd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let l = Layout {
            updates: d.join("updates"),
            versions: d.join("versions"),
        };
        (d, l)
    }

    #[test]
    fn versions_compare() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("v0.2.2") > v("0.2.1"));
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("1.0.0-rc.2"));
        assert!(v("1.0.0-rc.10") > v("1.0.0-rc.2"));
        assert!(v("1.0.0-beta") > v("1.0.0-alpha.1"));
        assert_eq!(v("0.3"), v("0.3.0"));
        assert_eq!(v("1.2.3+build.5").to_string(), "1.2.3");
        assert!(Version::parse("1.2.3.4").is_none() && Version::parse("x").is_none());
    }

    #[test]
    fn only_https_github() {
        assert!(url_allowed(
            "https://github.com/daniel-farina/godterm/releases/download/v1/x.zip",
            false
        ));
        assert!(url_allowed(
            "https://objects.githubusercontent.com/a/b",
            false
        ));
        assert!(!url_allowed("http://github.com/x", false));
        assert!(!url_allowed("https://github.com.evil.com/x", false));
        assert!(!url_allowed("https://evil.com@github.com/x", false));
        assert!(!url_allowed("http://127.0.0.1:9/x", false));
        assert!(url_allowed("http://127.0.0.1:9/x", true));
    }

    #[test]
    fn detects_install_methods() {
        let (d, l) = tmp("detect");
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(
            detect(
                &p("/opt/homebrew/bin/godterm"),
                &p("/opt/homebrew/Cellar/godterm/0.2.1/bin/godterm"),
                None,
                &l
            ),
            Method::Homebrew
        );
        assert!(matches!(
            detect(
                &p("/x/godterm"),
                &p("/home/u/claudego/target/release/godterm"),
                None,
                &l
            ),
            Method::Dev { .. }
        ));
        assert_eq!(
            detect(
                &p("/Applications/GodTerm.app/Contents/MacOS/godterm"),
                &p("/Applications/GodTerm.app/Contents/MacOS/godterm"),
                None,
                &l
            ),
            Method::AppBundle {
                app: p("/Applications/GodTerm.app")
            }
        );
        assert_eq!(
            detect(
                &p("/tmp/.mount/godterm"),
                &p("/tmp/.mount/usr/bin/godterm"),
                Some(&p("/home/u/GodTerm.AppImage")),
                &l
            ),
            Method::AppImage {
                path: p("/home/u/GodTerm.AppImage")
            }
        );
        if cfg!(target_os = "linux") {
            assert!(matches!(
                detect(&p("/usr/bin/godterm"), &p("/usr/bin/godterm"), None, &l),
                Method::System { .. }
            ));
        }
        // install.sh: a plain file in a writable bin folder.
        let bin = d.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("godterm"), "old").unwrap();
        assert_eq!(
            detect(&bin.join("godterm"), &bin.join("godterm"), None, &l),
            Method::Standalone {
                path: bin.join("godterm")
            }
        );
        // After an update: a link into the versions folder.
        let v = l.versions.join("0.2.2");
        std::fs::create_dir_all(&v).unwrap();
        std::fs::write(v.join("godterm"), "new").unwrap();
        // (Windows needs a privilege for symlinks.)
        if !cfg!(windows) {
            let link = d.join("link-godterm");
            crate::platform::symlink(&v.join("godterm"), &link).unwrap();
            assert_eq!(
                detect(&link, &v.join("godterm"), None, &l),
                Method::Managed { link: link.clone() }
            );
        }
        assert_eq!(Method::Homebrew.hint().unwrap(), "brew upgrade godterm");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn check_stage_install_and_roll_back() {
        let (d, l) = tmp("flow");
        let bin_dir = d.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let target = bin_dir.join(exe_name());
        std::fs::write(&target, "old build").unwrap();
        let method = test_method(target.clone());
        let alias = "godterm-test.tar.gz".to_string();
        // Newer, signed; the first archive download is cut off.
        let srv = mock_routes(|base| routes(base, "v9.9.9", true, &sums_for(&alias)), true);
        let src = test_source(&srv.base);
        let mut cache = Cache::default();
        let rel = check(&src, &mut cache, "stable").unwrap().unwrap();
        assert_eq!(rel.version().to_string(), "9.9.9");
        assert_eq!(cache.etag.as_deref(), Some("\"e1\""));
        assert_eq!(rel.notes(5), vec!["What's new", "Faster restarts", "Fixes"]);
        // The ETag: a 304 answers from the cache.
        let again = check(&src, &mut cache, "stable").unwrap().unwrap();
        assert_eq!(again.tag, "v9.9.9");
        // The first download is cut off halfway; the second resumes.
        assert!(stage(&src, &rel, &method, &l, &|_, _| {}).is_err());
        let staged = stage(&src, &rel, &method, &l, &|_, _| {}).unwrap();
        assert!(
            srv.hits
                .lock()
                .unwrap()
                .iter()
                .any(|h| h.contains(" range=")),
            "resumed with a Range"
        );
        let Payload::Binary { bin, .. } = &staged.payload else {
            panic!()
        };
        assert!(std::fs::read_to_string(bin)
            .unwrap()
            .contains("godterm 9.9.9"));
        // Install: the target now runs 9.9.9, the old build is kept.
        let cur = Version::parse("0.2.1").unwrap();
        let next = install(&staged, &method, &cur, false, &l).unwrap();
        assert_eq!(next.program, target);
        assert!(std::fs::read_to_string(&target)
            .unwrap()
            .contains("godterm 9.9.9"));
        if !cfg!(windows) {
            assert!(target.is_symlink());
        }
        assert_eq!(
            std::fs::read_to_string(l.versions.join("0.2.1").join(exe_name())).unwrap(),
            "old build"
        );
        assert!(!staged.dir.exists(), "the download is cleaned up");
        // Rollback.
        assert_eq!(rollback(&l).unwrap(), "0.2.1");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old build");
        let rec = Installed::load(&l);
        assert_eq!(rec.active.as_deref(), Some("0.2.1"));
        // A downgrade is refused unless forced.
        let staged2 = stage(&src, &rel, &method, &l, &|_, _| {}).unwrap();
        let newer = Version::parse("10.0.0").unwrap();
        assert!(install(&staged2, &method, &newer, false, &l)
            .unwrap_err()
            .to_string()
            .contains("refused"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn refuses_bad_checksums_and_signatures() {
        let (d, l) = tmp("refuse");
        let method = test_method(d.join("godterm"));
        let alias = "godterm-test.tar.gz".to_string();
        // Wrong hash in SHA256SUMS (unsigned, so only the checksum stops it).
        let bad_sums = format!("{}  {alias}\n", "ab".repeat(32)).into_bytes();
        let srv = mock_routes(|b| routes(b, "v9.9.9", false, &bad_sums), false);
        let src = test_source(&srv.base);
        let rel = check(&src, &mut Cache::default(), "stable")
            .unwrap()
            .unwrap();
        let e = stage(&src, &rel, &method, &l, &|_, _| {})
            .unwrap_err()
            .to_string();
        assert!(e.contains("checksum mismatch"), "{e}");
        assert!(
            !l.updates.join("9.9.9").join(&alias).exists(),
            "nothing left behind"
        );
        // A signature that does not match the (edited) SHA256SUMS.
        let mut edited = sums_for(&alias);
        edited.extend_from_slice(format!("{}  evil.zip\n", "cd".repeat(32)).as_bytes());
        let srv = mock_routes(|b| routes(b, "v9.9.9", true, &edited), false);
        let src = test_source(&srv.base);
        let rel = check(&src, &mut Cache::default(), "stable")
            .unwrap()
            .unwrap();
        let e = format!(
            "{:#}",
            stage(&src, &rel, &method, &l, &|_, _| {}).unwrap_err()
        );
        assert!(e.contains("refused") && e.contains("signature"), "{e}");
        // Unsigned while signatures are required.
        let srv = mock_routes(|b| routes(b, "v9.9.9", false, &sums_for(&alias)), false);
        let mut src = test_source(&srv.base);
        src.require_sig = true;
        let rel = check(&src, &mut Cache::default(), "stable")
            .unwrap()
            .unwrap();
        let e = stage(&src, &rel, &method, &l, &|_, _| {})
            .unwrap_err()
            .to_string();
        assert!(e.contains("not signed"), "{e}");
        // The signature itself verifies over the original file.
        verify_sig(SUMS, SIG, &test_pubkey()).unwrap();
        assert!(verify_sig(b"tampered", SIG, &test_pubkey()).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn signatures_are_required_by_default() {
        assert!(crate::config::UpdateCfg::default().require_signature);
        assert!(Source::github(true).require_sig);
    }

    /// An unsigned release while signatures are required: the check still
    /// reports it, staging refuses before any download with a message that
    /// names the override, and nothing is left on disk.
    #[test]
    fn unsigned_release_is_reported_but_never_installed() {
        let (d, l) = tmp("unsigned");
        let method = test_method(d.join("godterm"));
        let alias = "godterm-test.tar.gz".to_string();
        let srv = mock_routes(|b| routes(b, "v9.9.9", false, &sums_for(&alias)), false);
        let mut src = test_source(&srv.base);
        src.require_sig = true;
        let rel = check(&src, &mut Cache::default(), "stable")
            .unwrap()
            .expect("the check still sees the release");
        assert_eq!(rel.version().to_string(), "9.9.9");
        let e = stage(&src, &rel, &method, &l, &|_, _| {})
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("release v9.9.9 is not signed; refusing to update")
                && e.contains("update.require_signature = false"),
            "{e}"
        );
        assert!(
            !l.updates.join("9.9.9").exists(),
            "nothing downloaded or left behind"
        );
        assert!(!l.versions.join("9.9.9").exists(), "nothing installed");
        // --force only lifts the downgrade guard in install(); staging (where
        // the signature is checked) takes no force flag, so it cannot skip it.
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn up_to_date_and_rate_limits() {
        let cur = Version::current();
        let srv = mock_routes(
            |b| {
                vec![(
                    format!("/repos/{REPO}/releases/latest"),
                    release_json(b, &format!("v{cur}"), true),
                )]
            },
            false,
        );
        let src = test_source(&srv.base);
        let rel = check(&src, &mut Cache::default(), "stable")
            .unwrap()
            .unwrap();
        assert_eq!(rel.version(), cur, "up to date");
        // No release yet: 404 is "none".
        let mut none = test_source(&srv.base);
        none.repo = "other/repo".into();
        assert!(check(&none, &mut Cache::default(), "stable")
            .unwrap()
            .is_none());
        // 429: back off, and do not ask again before it is over.
        let mut lim = test_source(&srv.base);
        lim.repo = "x/limited".into();
        let mut cache = Cache::default();
        assert!(check(&lim, &mut cache, "stable").is_err());
        assert!(cache.backoff_until.unwrap() > now_s() + 100);
        let e = check(&src, &mut cache, "stable").unwrap_err().to_string();
        assert!(e.contains("rate limit"), "{e}");
    }
}
