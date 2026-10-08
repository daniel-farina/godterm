//! What GodTerm needs, what is there, and how to get the rest: Claude Code
//! (required), Grok Build (optional) and the local voice pack (ffmpeg,
//! whisper.cpp and a model, espeak-ng and the Kokoro files, the speaker
//! model). Each item has a status and a plan per platform: the package
//! manager there (brew, apt, dnf, pacman, winget), an official installer,
//! a pinned and checksummed download, or plain words when nothing fits.
//! `godterm doctor`, the Setup screen and the assistant all read this.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Group {
    Required,
    Optional,
    Voice,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Status {
    Ok(String),
    Missing(String),
    Broken(String),
}

impl Status {
    pub fn ok(&self) -> bool {
        matches!(self, Status::Ok(_))
    }
    pub fn word(&self) -> &'static str {
        match self {
            Status::Ok(_) => "ok",
            Status::Missing(_) => "missing",
            Status::Broken(_) => "broken",
        }
    }
    pub fn detail(&self) -> &str {
        match self {
            Status::Ok(s) | Status::Missing(s) | Status::Broken(s) => s,
        }
    }
}

/// A pinned file: fetched over HTTPS from a known host, checked against
/// its SHA-256 before it is moved into place.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Download {
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub dest: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Step {
    /// A program and its arguments (a package manager).
    Run {
        program: String,
        args: Vec<String>,
        sudo: bool,
    },
    /// An official install script, through the shell.
    Script {
        shell: String,
        line: String,
    },
    Download(Download),
    /// Plain instructions.
    Manual(String),
}

impl Step {
    /// As it reads in a confirmation.
    pub fn shown(&self) -> String {
        match self {
            Step::Run {
                program,
                args,
                sudo,
            } => format!(
                "{}{program} {}",
                if *sudo { "sudo " } else { "" },
                args.join(" ")
            ),
            Step::Script { line, .. } => line.clone(),
            Step::Download(d) => format!(
                "download {} ({}) to {}",
                d.url,
                size(d.bytes),
                crate::picker::tilde(&d.dest.to_string_lossy())
            ),
            Step::Manual(t) => t.clone(),
        }
    }

    pub fn needs_sudo(&self) -> bool {
        matches!(self, Step::Run { sudo: true, .. })
    }
}

pub fn size(b: u64) -> String {
    if b >= 1_000_000_000 {
        format!("{:.1} GB", b as f64 / 1e9)
    } else {
        format!("{} MB", b.div_ceil(1_000_000))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dep {
    pub id: &'static str,
    pub name: &'static str,
    pub group: Group,
    pub status: Status,
    pub steps: Vec<Step>,
    pub why: &'static str,
}

impl Dep {
    pub fn download_bytes(&self) -> u64 {
        self.steps
            .iter()
            .map(|s| match s {
                Step::Download(d) => d.bytes,
                _ => 0,
            })
            .sum()
    }
    /// Steps GodTerm can run (not just words or a link).
    pub fn installable(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(|s| !matches!(s, Step::Manual(_)))
    }
}

/// The package manager here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pm {
    Brew,
    Apt,
    Dnf,
    Pacman,
    Winget,
    None,
}

/// The operating system a plan is for (the host, or one a test picks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Mac,
    Linux,
    Windows,
}

impl Os {
    pub fn host() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }
}

/// What the status checks look at: programs (`which`), the platform and
/// the cache folder for the models. The host's, or a test's.
pub struct Probe<'a> {
    pub which: &'a dyn Fn(&str) -> Option<PathBuf>,
    pub os: Os,
    /// ~/.cache (whisper-models, kokoro-onnx, godterm-models live here).
    pub cache: PathBuf,
}

impl<'a> Probe<'a> {
    pub fn host(which: &'a dyn Fn(&str) -> Option<PathBuf>) -> Probe<'a> {
        Probe {
            which,
            os: Os::host(),
            cache: crate::config::home_dir().join(".cache"),
        }
    }
}

pub fn package_manager(which: &dyn Fn(&str) -> Option<PathBuf>, os: Os) -> Pm {
    if os == Os::Windows {
        return if which("winget").is_some() {
            Pm::Winget
        } else {
            Pm::None
        };
    }
    for (b, p) in [
        ("brew", Pm::Brew),
        ("apt-get", Pm::Apt),
        ("dnf", Pm::Dnf),
        ("pacman", Pm::Pacman),
    ] {
        if which(b).is_some() {
            // Linux with Homebrew too: the system manager is preferred.
            if p == Pm::Brew
                && os == Os::Linux
                && which("apt-get").or_else(|| which("dnf")).is_some()
            {
                continue;
            }
            return p;
        }
    }
    Pm::None
}

/// `program` on PATH (or a path), honoring .exe on Windows.
pub fn which(bin: &str) -> Option<PathBuf> {
    let p = crate::config::expand_tilde(bin);
    if p.components().count() > 1 {
        return p.is_file().then_some(p);
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|d| {
        let c = d.join(bin);
        if c.is_file() {
            return Some(c);
        }
        let e = d.join(format!("{bin}.exe"));
        (cfg!(windows) && e.is_file()).then_some(e)
    })
}

/// A package by manager: (brew, apt, dnf, pacman, winget) names.
fn pkg(pm: Pm, names: [Option<&str>; 5]) -> Option<Step> {
    let (program, args, sudo, name) = match pm {
        Pm::Brew => ("brew", vec!["install"], false, names[0]?),
        Pm::Apt => ("apt-get", vec!["install", "-y"], true, names[1]?),
        Pm::Dnf => ("dnf", vec!["install", "-y"], true, names[2]?),
        Pm::Pacman => ("pacman", vec!["-S", "--noconfirm"], true, names[3]?),
        Pm::Winget => ("winget", vec!["install", "-e", "--id"], false, names[4]?),
        Pm::None => return None,
    };
    let mut a: Vec<String> = args.into_iter().map(str::to_string).collect();
    a.push(name.to_string());
    Some(Step::Run {
        program: program.into(),
        args: a,
        sudo,
    })
}

fn script(os: Os, unix: &str, windows: &str) -> Step {
    if os == Os::Windows {
        Step::Script {
            shell: "powershell".into(),
            line: windows.into(),
        }
    } else {
        Step::Script {
            shell: "sh".into(),
            line: unix.into(),
        }
    }
}

/// Whisper models offered (the first is recommended).
pub const WHISPER_MODELS: &[(&str, &str, &str, u64, &str)] = &[
    (
        "large-v3-turbo",
        "ggml-large-v3-turbo.bin",
        "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
        1_624_555_275,
        "recommended: accurate and fast on Apple silicon",
    ),
    (
        "small.en",
        "ggml-small.en.bin",
        "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
        487_614_201,
        "smaller: English only, less accurate",
    ),
];

pub const WHISPER_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";
pub const KOKORO_BASE: &str =
    "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/";
pub const KOKORO_FILES: &[(&str, &str, u64)] = &[
    (
        "kokoro-v1.0.onnx",
        "7d5df8ecf7d4b1878015a32686053fd0eebe2bc377234608764cc0ef3636a6c5",
        325_532_387,
    ),
    (
        "voices-v1.0.bin",
        "bca610b8308e8d99f32e6fe4197e7ec01679264efed0cac9140fe9c29f1fbf7d",
        28_214_398,
    ),
];

/// Everything, with its status and plan. `whisper_choice` picks the
/// model to offer (default: the recommended one).
pub fn all(cfg: &crate::config::Config, whisper_choice: Option<&str>, probe: &Probe) -> Vec<Dep> {
    let which = probe.which;
    let os = probe.os;
    let pm = package_manager(which, os);
    let vc = &cfg.voice;
    let have = |b: &str| which(b).map(|p| p.display().to_string());
    let mut v = vec![];
    let manual = |t: &str| Step::Manual(t.to_string());
    // Claude Code: the official installer only.
    let claude = cfg.claude_bin();
    v.push(Dep {
        id: "claude",
        name: "Claude Code",
        group: Group::Required,
        status: match have(&claude) {
            Some(p) => Status::Ok(p),
            None => Status::Missing(format!("{claude} not found")),
        },
        steps: vec![script(
            os,
            "curl -fsSL https://claude.ai/install.sh | bash",
            "irm https://claude.ai/install.ps1 | iex",
        )],
        why: "every tab runs it",
    });
    // Grok Build: its documented installer.
    let grok = crate::harness::Harness::Grok.bin(cfg.grok_bin.as_deref());
    v.push(Dep {
        id: "grok",
        name: "Grok Build",
        group: Group::Optional,
        status: match have(&grok) {
            Some(p) => Status::Ok(p),
            None => Status::Missing("not installed".into()),
        },
        steps: vec![script(
            os,
            "curl -fsSL https://x.ai/cli/install.sh | bash",
            "irm https://x.ai/cli/install.ps1 | iex",
        )],
        why: "Grok accounts, and the assistant on Grok",
    });
    let or_manual = |s: Option<Step>, t: &str| s.unwrap_or_else(|| manual(t));
    v.push(Dep {
        id: "ffmpeg",
        name: "ffmpeg",
        group: Group::Voice,
        status: match have(&vc.ffmpeg) {
            Some(p) => Status::Ok(p),
            None => Status::Missing("not found".into()),
        },
        steps: vec![or_manual(
            pkg(
                pm,
                [
                    Some("ffmpeg"),
                    Some("ffmpeg"),
                    Some("ffmpeg"),
                    Some("ffmpeg"),
                    Some("Gyan.FFmpeg"),
                ],
            ),
            "install ffmpeg from ffmpeg.org and put it on PATH",
        )],
        why: "the microphone",
    });
    let whisper = have(&vc.whisper_server)
        .or_else(|| have(&vc.whisper_cli))
        .or_else(|| have("whisper-server"))
        .or_else(|| have("whisper-cli"));
    v.push(Dep {
        id: "whisper",
        name: "whisper.cpp",
        group: Group::Voice,
        status: match whisper {
            Some(p) => Status::Ok(p),
            None => Status::Missing("whisper-server not found".into()),
        },
        steps: vec![or_manual(
            pkg(pm, [Some("whisper-cpp"), None, None, None, None]),
            "build whisper.cpp from github.com/ggml-org/whisper.cpp (cmake) and set voice.whisper_server",
        )],
        why: "speech recognition on this machine",
    });
    let model_path = crate::config::expand_tilde(&vc.model);
    let m = whisper_choice
        .and_then(|c| WHISPER_MODELS.iter().find(|m| m.0 == c))
        .unwrap_or(&WHISPER_MODELS[0]);
    let dir = model_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| probe.cache.join("whisper-models"));
    v.push(Dep {
        id: "whisper-model",
        name: "Whisper model",
        group: Group::Voice,
        status: match std::fs::metadata(&model_path) {
            Ok(md) if md.len() > 1_000_000 => Status::Ok(format!(
                "{} ({})",
                crate::picker::tilde(&model_path.to_string_lossy()),
                size(md.len())
            )),
            Ok(_) => Status::Broken(format!("{} is too small", model_path.display())),
            Err(_) => Status::Missing(format!(
                "{} missing",
                crate::picker::tilde(&model_path.to_string_lossy())
            )),
        },
        steps: vec![Step::Download(Download {
            url: format!("{WHISPER_BASE}{}", m.1),
            sha256: m.2.into(),
            bytes: m.3,
            dest: dir.join(m.1),
        })],
        why: m.4,
    });
    v.push(Dep {
        id: "espeak",
        name: "espeak-ng",
        group: Group::Voice,
        status: match have(&vc.espeak).or_else(|| have("espeak-ng")) {
            Some(p) => Status::Ok(p),
            None => Status::Missing("not found".into()),
        },
        steps: vec![or_manual(
            pkg(
                pm,
                [
                    Some("espeak-ng"),
                    Some("espeak-ng"),
                    Some("espeak-ng"),
                    Some("espeak-ng"),
                    None,
                ],
            ),
            "install espeak-ng from github.com/espeak-ng/espeak-ng/releases",
        )],
        why: "pronunciation for the Kokoro voice",
    });
    let kdir = crate::config::expand_tilde(&vc.kokoro_model)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| probe.cache.join("kokoro-onnx"));
    let kmissing: Vec<&str> = [
        crate::config::expand_tilde(&vc.kokoro_model),
        crate::config::expand_tilde(&vc.kokoro_voices),
    ]
    .iter()
    .zip(KOKORO_FILES)
    .filter(|(p, _)| !p.is_file())
    .map(|(_, k)| k.0)
    .collect();
    v.push(Dep {
        id: "kokoro",
        name: "Kokoro voice files",
        group: Group::Voice,
        status: if kmissing.is_empty() {
            Status::Ok(crate::picker::tilde(&kdir.to_string_lossy()))
        } else {
            Status::Missing(format!("{} missing", kmissing.join(", ")))
        },
        steps: KOKORO_FILES
            .iter()
            .map(|(f, sha, b)| {
                Step::Download(Download {
                    url: format!("{KOKORO_BASE}{f}"),
                    sha256: sha.to_string(),
                    bytes: *b,
                    dest: kdir.join(f),
                })
            })
            .collect(),
        why: "the natural local talk back voice",
    });
    let sm = &crate::voice::speaker::MODELS[0];
    let sp = probe.cache.join("godterm-models").join(sm.file);
    v.push(Dep {
        id: "speaker",
        name: "Speaker model",
        group: Group::Voice,
        status: if sp.is_file() {
            Status::Ok(crate::picker::tilde(&sp.to_string_lossy()))
        } else {
            Status::Missing("not installed".into())
        },
        steps: vec![Step::Download(Download {
            url: sm.url.into(),
            sha256: sm.sha256.into(),
            bytes: sm.bytes,
            dest: sp,
        })],
        why: "the speaker lock (only your voice)",
    });
    v
}

/// Targets as said: "voice pack", "claude", "grok", or an item id.
pub fn resolve(targets: &str, deps: &[Dep]) -> Vec<&'static str> {
    let t = targets.to_lowercase();
    let t = t.trim();
    if t.contains("voice") {
        return deps
            .iter()
            .filter(|d| d.group == Group::Voice)
            .map(|d| d.id)
            .collect();
    }
    if t == "all" || t == "everything" || t == "missing" {
        return deps
            .iter()
            .filter(|d| !d.status.ok())
            .map(|d| d.id)
            .collect();
    }
    deps.iter()
        .filter(|d| {
            t.split([',', ' ']).any(|w| {
                !w.is_empty()
                    && (d.id == w
                        || d.name.to_lowercase().starts_with(w)
                        || (w == "claude" && d.id == "claude"))
            })
        })
        .map(|d| d.id)
        .collect()
}

// ---------- downloads ----------

const HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "huggingface.co",
];

fn host_ok(url: &str, local_ok: bool) -> bool {
    if local_ok && url.starts_with("http://127.0.0.1:") {
        return true;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#', ':'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    !rest.split('/').next().unwrap_or("").contains('@')
        && (HOSTS.contains(&host.as_str())
            || host.ends_with(".huggingface.co")
            || host.ends_with(".hf.co")
            || host.ends_with(".xethub.hf.co"))
}

/// Free bytes where `dir` is (None when unknown).
pub fn free_bytes(dir: &Path) -> Option<u64> {
    let mut d = dir.to_path_buf();
    while !d.exists() {
        d = d.parent()?.to_path_buf();
    }
    if cfg!(windows) {
        return None;
    }
    let o = crate::procs::output_timeout(
        std::process::Command::new("df").arg("-Pk").arg(&d),
        Duration::from_secs(5),
    )
    .ok()?;
    let t = String::from_utf8_lossy(&o.stdout);
    let kb: u64 = t.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()?;
    Some(kb * 1024)
}

pub fn sha256_file(p: &Path) -> Result<String> {
    crate::voice::speaker::sha256_file(p)
}

/// Fetch `d` (resuming a `.part`, every redirect checked), check its
/// hash, then move it into place. A mismatch deletes it and refuses.
pub fn download(d: &Download, local_ok: bool, progress: &dyn Fn(u64, u64)) -> Result<()> {
    if d.dest.is_file() && sha256_file(&d.dest)? == d.sha256 {
        return Ok(());
    }
    if let Some(dir) = d.dest.parent() {
        std::fs::create_dir_all(dir)?;
        if let Some(free) = free_bytes(dir) {
            if free < d.bytes + 200_000_000 {
                bail!(
                    "not enough disk space for {} ({} free, {} needed)",
                    d.dest.display(),
                    size(free),
                    size(d.bytes)
                );
            }
        }
    }
    let part = PathBuf::from(format!("{}.part", d.dest.display()));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let mut tries = 0;
    loop {
        tries += 1;
        let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let r = (|| -> Result<()> {
            let mut url = d.url.clone();
            let mut resp = None;
            for _ in 0..8 {
                if !host_ok(&url, local_ok) {
                    bail!("refused to download from {url} (not a known HTTPS host)");
                }
                let mut req = agent.get(&url);
                if have > 0 {
                    req = req.header("Range", &format!("bytes={have}-"));
                }
                let r = req.call().map_err(|e| anyhow!("{url}: {e}"))?;
                let c = r.status().as_u16();
                if matches!(c, 301 | 302 | 303 | 307 | 308) {
                    let loc = r
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
                resp = Some(r);
                break;
            }
            let mut r = resp.ok_or_else(|| anyhow!("too many redirects"))?;
            let code = r.status().as_u16();
            let (mut f, start) = match code {
                206 if have > 0 => (std::fs::OpenOptions::new().append(true).open(&part)?, have),
                200 => (std::fs::File::create(&part)?, 0),
                c => bail!("the download answered {c}"),
            };
            let len = r
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let total = if len > 0 { start + len } else { d.bytes };
            let mut body = r.body_mut().with_config().limit(1 << 36).reader();
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
            if got < total {
                bail!("the download was cut off at {got} of {total} bytes");
            }
            Ok(())
        })();
        match r {
            Ok(()) => break,
            Err(e) if tries < 3 && !e.to_string().contains("refused") => {
                crate::log::info(&format!(
                    "setup: download of {} failed ({e:#}); retrying",
                    d.url
                ));
                std::thread::sleep(Duration::from_millis(if cfg!(test) { 10 } else { 1500 }));
            }
            Err(e) => return Err(e),
        }
    }
    let got = sha256_file(&part)?;
    if got != d.sha256 {
        let _ = std::fs::remove_file(&part);
        bail!(
            "refused: checksum mismatch for {} (expected {}, got {got})",
            d.dest.display(),
            d.sha256
        );
    }
    std::fs::rename(&part, &d.dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every plan from what a test sets: the platform, the programs on
    /// its PATH and its cache folder; nothing of the host's.
    fn hermetic(os: Os, tools: &'static [&'static str], tag: &str) -> Vec<Dep> {
        let home = std::env::temp_dir().join(format!("godterm-deps-{tag}-{}", std::process::id()));
        let mut cfg = crate::config::Config::default();
        cfg.claude_bin = Some("claude".into());
        cfg.grok_bin = Some("grok".into());
        cfg.voice.ffmpeg = "ffmpeg".into();
        cfg.voice.whisper_server = "whisper-server".into();
        cfg.voice.whisper_cli = "whisper-cli".into();
        cfg.voice.espeak = "espeak-ng".into();
        cfg.voice.model = home
            .join("cache/whisper-models/ggml-large-v3-turbo.bin")
            .display()
            .to_string();
        cfg.voice.kokoro_model = home
            .join("cache/kokoro-onnx/kokoro-v1.0.onnx")
            .display()
            .to_string();
        cfg.voice.kokoro_voices = home
            .join("cache/kokoro-onnx/voices-v1.0.bin")
            .display()
            .to_string();
        let which = move |b: &str| {
            tools
                .contains(&b)
                .then(|| PathBuf::from(format!("/fake/bin/{b}")))
        };
        let probe = Probe {
            which: &which,
            os,
            cache: home.join("cache"),
        };
        all(&cfg, Some("small.en"), &probe)
    }

    fn plan(d: &[Dep], id: &str) -> Vec<String> {
        d.iter()
            .find(|x| x.id == id)
            .unwrap()
            .steps
            .iter()
            .map(Step::shown)
            .collect()
    }

    #[test]
    fn package_managers_by_platform() {
        let w = |set: &'static [&'static str]| {
            move |b: &str| set.contains(&b).then(|| PathBuf::from(format!("/bin/{b}")))
        };
        assert_eq!(package_manager(&w(&["brew"]), Os::Mac), Pm::Brew);
        assert_eq!(package_manager(&w(&["brew"]), Os::Windows), Pm::None);
        assert_eq!(package_manager(&w(&["winget"]), Os::Windows), Pm::Winget);
        assert_eq!(
            package_manager(&w(&["brew", "apt-get"]), Os::Linux),
            Pm::Apt
        );
        assert_eq!(package_manager(&w(&["dnf"]), Os::Linux), Pm::Dnf);
        assert_eq!(package_manager(&w(&[]), Os::Linux), Pm::None);
        let s = pkg(Pm::Apt, [Some("ffmpeg"), Some("ffmpeg"), None, None, None]).unwrap();
        assert!(s.needs_sudo());
        assert_eq!(s.shown(), "sudo apt-get install -y ffmpeg");
        assert!(pkg(Pm::Dnf, [None, None, None, None, None]).is_none());
        assert!(
            host_ok("https://huggingface.co/x", false)
                && host_ok("https://cdn-lfs.huggingface.co/x", false)
        );
        assert!(
            !host_ok("http://huggingface.co/x", false) && !host_ok("https://evil.com/x", false)
        );
    }

    #[test]
    fn plans_on_a_mac_with_brew() {
        let d = hermetic(Os::Mac, &["brew"], "mac");
        assert!(
            d.iter().all(|x| !x.status.ok()),
            "nothing of the host's counts"
        );
        assert_eq!(
            plan(&d, "claude"),
            vec!["curl -fsSL https://claude.ai/install.sh | bash"]
        );
        assert_eq!(
            plan(&d, "grok"),
            vec!["curl -fsSL https://x.ai/cli/install.sh | bash"]
        );
        assert_eq!(plan(&d, "ffmpeg"), vec!["brew install ffmpeg"]);
        assert_eq!(plan(&d, "whisper"), vec!["brew install whisper-cpp"]);
        assert_eq!(plan(&d, "espeak"), vec!["brew install espeak-ng"]);
        let m = d.iter().find(|x| x.id == "whisper-model").unwrap();
        assert!(
            m.steps[0].shown().contains("ggml-small.en.bin") && m.download_bytes() == 487_614_201
        );
        assert_eq!(resolve("voice pack", &d).len(), 6);
        assert_eq!(resolve("claude", &d), vec!["claude"]);
        let voice: u64 = d
            .iter()
            .filter(|x| x.group == Group::Voice)
            .map(Dep::download_bytes)
            .sum();
        assert_eq!(size(voice), "868 MB");
        // A tool on the injected PATH is found, nothing else.
        let ok = hermetic(Os::Mac, &["brew", "ffmpeg", "claude"], "mac2");
        assert!(ok.iter().find(|x| x.id == "ffmpeg").unwrap().status.ok());
        assert!(ok.iter().find(|x| x.id == "claude").unwrap().status.ok());
    }

    #[test]
    fn plans_on_linux_with_apt() {
        let d = hermetic(Os::Linux, &["apt-get"], "apt");
        assert_eq!(plan(&d, "ffmpeg"), vec!["sudo apt-get install -y ffmpeg"]);
        assert_eq!(
            plan(&d, "espeak"),
            vec!["sudo apt-get install -y espeak-ng"]
        );
        let w = d.iter().find(|x| x.id == "whisper").unwrap();
        assert!(!w.installable() && plan(&d, "whisper")[0].starts_with("build whisper.cpp"));
        assert_eq!(
            plan(&d, "claude"),
            vec!["curl -fsSL https://claude.ai/install.sh | bash"]
        );
    }

    #[test]
    fn plans_on_windows_with_winget() {
        let d = hermetic(Os::Windows, &["winget"], "win");
        assert_eq!(
            plan(&d, "claude"),
            vec!["irm https://claude.ai/install.ps1 | iex"]
        );
        assert_eq!(
            plan(&d, "grok"),
            vec!["irm https://x.ai/cli/install.ps1 | iex"]
        );
        assert_eq!(
            plan(&d, "ffmpeg"),
            vec!["winget install -e --id Gyan.FFmpeg"]
        );
        assert!(!d.iter().find(|x| x.id == "espeak").unwrap().installable());
    }
}
