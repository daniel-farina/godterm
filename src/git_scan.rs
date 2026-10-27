use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::git_ops::{self, Ctx};

pub const WARN_BYTES: u64 = 50 * 1024 * 1024;
pub const BLOCK_BYTES: u64 = 95 * 1024 * 1024;
pub const TOTAL_BYTES: u64 = 1536 * 1024 * 1024;

pub const OVERRIDE: &str = "push anyway";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Finding {
    pub file: String,
    pub line: u32,
    pub rule: String,

    pub redacted: String,
}

impl Finding {
    pub fn line_text(&self) -> String {
        format!(
            "{}:{} {} {}",
            self.file, self.line, self.rule, self.redacted
        )
    }
}

fn patterns() -> &'static [(&'static str, Regex)] {
    static P: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    P.get_or_init(|| {
        [
            ("aws-access-key", r"AKIA[0-9A-Z]{16}"),
            ("github-token", r"gh[pousr]_[A-Za-z0-9]{36,}"),
            ("github-pat", r"github_pat_[A-Za-z0-9_]{80,}"),
            ("anthropic-key", r"sk-ant-[A-Za-z0-9_-]{20,}"),
            ("openai-key", r"sk-(?:proj-)?[A-Za-z0-9_-]{20,}"),
            ("xai-key", r"xai-[A-Za-z0-9]{20,}"),
            ("google-api-key", r"AIza[0-9A-Za-z_-]{35}"),
            ("slack-token", r"xox[abprs]-[A-Za-z0-9-]{10,}"),
            ("stripe-live-key", r"[sr]k_live_[A-Za-z0-9]{20,}"),
            ("private-key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
            (
                "generic-secret",
                r#"(?i)(?:api[_-]?key|secret|token|passw(?:or)?d)\s*[:=]\s*['"]?[A-Za-z0-9/+_.-]{16,}"#,
            ),
        ]
        .into_iter()
        .map(|(n, r)| (n, Regex::new(r).expect("secret pattern")))
        .collect()
    })
}

pub fn redact(s: &str) -> String {
    let n = s.chars().count();
    if n <= 8 {
        return "*".repeat(n.max(4));
    }
    let head: String = s.chars().take(4).collect();
    format!("{head}****({n} chars)")
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn scan_text(file: &str, text: &str, first: u32) -> Vec<Finding> {
    let mut v = vec![];
    for (i, l) in text.lines().enumerate() {
        v.extend(scan_line(file, l, first + i as u32));
    }
    v
}

fn scan_line(file: &str, l: &str, line: u32) -> Vec<Finding> {
    let mut v: Vec<Finding> = vec![];
    let mut taken: Vec<(usize, usize)> = vec![];
    for (rule, re) in patterns() {
        for m in re.find_iter(l) {
            if taken.iter().any(|(a, b)| m.start() < *b && *a < m.end()) {
                continue;
            }
            taken.push((m.start(), m.end()));
            v.push(Finding {
                file: file.to_string(),
                line,
                rule: rule.to_string(),
                redacted: redact(m.as_str()),
            });
        }
    }
    v
}

pub fn redact_text(text: &str) -> String {
    let mut out = text.to_string();
    for (_, re) in patterns() {
        out = re
            .replace_all(&out, |c: &regex::Captures| redact(&c[0]))
            .into_owned();
    }
    out
}

pub fn scan_patch(patch: &str) -> Vec<Finding> {
    let mut v = vec![];
    let mut file = String::new();
    let mut line: u32 = 0;
    for l in patch.lines() {
        if let Some(f) = l.strip_prefix("+++ ") {
            file = f.strip_prefix("b/").unwrap_or(f).to_string();
            continue;
        }
        if l.starts_with("--- ") {
            continue;
        }
        if let Some(h) = l.strip_prefix("@@ ") {
            line = h
                .split_whitespace()
                .find_map(|p| p.strip_prefix('+'))
                .and_then(|p| p.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(1);
            continue;
        }
        if let Some(added) = l.strip_prefix('+') {
            v.extend(scan_line(&file, added, line));
            line += 1;
        } else if !l.starts_with('-') && !l.starts_with('\\') {
            line += 1;
        }
    }
    v
}

fn ignored(top: &Path, f: &Finding) -> bool {
    let Ok(t) = std::fs::read_to_string(top.join(".gitleaksignore")) else {
        return false;
    };
    let tail = format!("{}:{}:{}", f.file, f.rule, f.line);
    t.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .any(|l| l == tail || l.ends_with(&format!(":{tail}")))
}

pub fn gitleaks_bin() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    ["/opt/homebrew/bin/gitleaks", "/usr/local/bin/gitleaks"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

pub fn gitleaks_args(dir: &Path, log_opts: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "git".into(),
        dir.to_string_lossy().into_owned(),
        "--redact".into(),
        "--no-banner".into(),
        "--report-format".into(),
        "json".into(),
        "--report-path".into(),
        "-".into(),
    ];
    if let Some(o) = log_opts {
        a.push(format!("--log-opts={o}"));
    }
    a
}

pub fn parse_gitleaks(json: &str) -> Result<Vec<Finding>, String> {
    let t = json.trim();
    if t.is_empty() {
        return Ok(vec![]);
    }
    let v: serde_json::Value =
        serde_json::from_str(t).map_err(|e| format!("gitleaks report: {e}"))?;
    let arr = v.as_array().ok_or("gitleaks report is not a list")?;
    Ok(arr
        .iter()
        .map(|x| {
            let secret = x["Secret"].as_str().or(x["Match"].as_str()).unwrap_or("");
            Finding {
                file: x["File"].as_str().unwrap_or("").to_string(),
                line: x["StartLine"].as_u64().unwrap_or(0) as u32,
                rule: x["RuleID"].as_str().unwrap_or("secret").to_string(),
                redacted: if secret.contains("REDACTED") {
                    "REDACTED".into()
                } else {
                    redact(secret)
                },
            }
        })
        .collect())
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct SecretScan {
    pub findings: Vec<Finding>,

    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn scan_outgoing(ctx: &Ctx, gitleaks: Option<&Path>) -> SecretScan {
    let range = git_ops::outgoing_range(ctx);
    let first = git_ops::upstream(ctx).is_none();
    if let Some(gl) = gitleaks {
        let opts = (!first).then(|| range.join(" "));
        let mut c = std::process::Command::new(gl);
        c.args(gitleaks_args(&ctx.top, opts.as_deref()));
        crate::project_git::quiet_env(&mut c);
        match crate::project_git::exec(c, std::time::Duration::from_secs(300), None) {
            Ok((_, out, err)) => match parse_gitleaks(&out) {
                Ok(f) => {
                    return SecretScan {
                        findings: f.into_iter().filter(|f| !ignored(&ctx.top, f)).collect(),
                        tool: "gitleaks".into(),
                        error: None,
                    }
                }
                Err(e) => crate::log::error(&format!(
                    "git_scan: gitleaks: {e} ({})",
                    crate::sessions::snippet(err.trim(), 200)
                )),
            },
            Err(e) => crate::log::error(&format!("git_scan: gitleaks: {e}")),
        }
    }
    let mut a: Vec<String> = vec![
        "log".into(),
        "-p".into(),
        "--no-color".into(),
        "--no-ext-diff".into(),
        "-U0".into(),
        "--format=".into(),
    ];
    a.extend(range);
    a.push("--".into());
    match git_ops::run(ctx, &a) {
        Ok(p) => SecretScan {
            findings: scan_patch(&p)
                .into_iter()
                .filter(|f| !ignored(&ctx.top, f))
                .collect(),
            tool: "built-in".into(),
            error: None,
        },

        Err(e) if git_ops::head(ctx).is_none() => SecretScan {
            tool: "built-in".into(),
            error: Some(e.to_string()).filter(|_| false),
            ..Default::default()
        },
        Err(e) => SecretScan {
            tool: "built-in".into(),
            error: Some(e.to_string()),
            ..Default::default()
        },
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Sizes {
    pub warn: Vec<(String, u64)>,

    pub block: Vec<(String, u64)>,
    pub total: u64,
    pub blobs: usize,
}

impl Sizes {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn too_big(&self) -> bool {
        !self.block.is_empty() || self.total > TOTAL_BYTES
    }
}

pub fn classify(blobs: &[(String, u64)]) -> Sizes {
    let mut s = Sizes {
        blobs: blobs.len(),
        ..Default::default()
    };
    for (p, n) in blobs {
        s.total += n;
        if *n > BLOCK_BYTES {
            s.block.push((p.clone(), *n));
        } else if *n > WARN_BYTES {
            s.warn.push((p.clone(), *n));
        }
    }
    s
}

pub fn outgoing_sizes(ctx: &Ctx) -> Result<Sizes, String> {
    if git_ops::head(ctx).is_none() {
        return Ok(Sizes::default());
    }
    let mut a: Vec<String> = vec!["rev-list".into(), "--objects".into()];
    a.extend(git_ops::outgoing_range(ctx));
    let objs = git_ops::run(ctx, &a).map_err(|e| e.to_string())?;
    if objs.trim().is_empty() {
        return Ok(Sizes::default());
    }
    let batch = git_ops::run_in(
        ctx,
        &[
            "cat-file".into(),
            "--batch-check=%(objecttype) %(objectsize) %(rest)".into(),
        ],
        Some(objs.as_bytes()),
    )
    .map_err(|e| e.to_string())?
    .0;
    let blobs: Vec<(String, u64)> = batch
        .lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, ' ');
            if it.next()? != "blob" {
                return None;
            }
            let n: u64 = it.next()?.parse().ok()?;
            Some((it.next().unwrap_or("").to_string(), n))
        })
        .collect();
    Ok(classify(&blobs))
}

pub fn big_worktree_files(ctx: &Ctx, paths: &[String]) -> Vec<(String, u64)> {
    paths
        .iter()
        .filter_map(|p| {
            let md = std::fs::symlink_metadata(ctx.top.join(p)).ok()?;
            (md.is_file() && md.len() > WARN_BYTES).then(|| (p.clone(), md.len()))
        })
        .collect()
}

pub fn mb(n: u64) -> String {
    format!("{:.0} MB", n as f64 / 1024.0 / 1024.0)
}

pub fn env_file(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    (n == ".env" || n.starts_with(".env."))
        && !matches!(n.as_str(), ".env.example" | ".env.sample" | ".env.template")
}

pub fn secret_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    env_file(name) || crate::guard::secret_name(name)
}

pub fn tracked_secret_files(ctx: &Ctx) -> Vec<String> {
    git_ops::run(ctx, &["ls-files".into(), "--cached".into()])
        .unwrap_or_default()
        .lines()
        .filter(|p| secret_file(p))
        .map(str::to_string)
        .collect()
}

pub fn bad_symlinks(ctx: &Ctx) -> Vec<(String, String)> {
    let t = git_ops::run(ctx, &["ls-files".into(), "--stage".into()]).unwrap_or_default();
    let mut v = vec![];
    for l in t.lines() {
        let Some((meta, path)) = l.split_once('\t') else {
            continue;
        };
        if !meta.starts_with("120000") {
            continue;
        }
        let at = ctx.top.join(path);
        let Ok(target) = std::fs::read_link(&at) else {
            continue;
        };
        let full = if target.is_absolute() {
            target
        } else {
            at.parent().unwrap_or(&ctx.top).join(target)
        };
        if let Some(why) = crate::guard::never_folder(&full) {
            v.push((path.to_string(), why));
        }
    }
    v
}

const WANTED: &[(&str, &str)] = &[
    (".env", ".env"),
    (".env.local", ".env.*"),
    ("x.pem", "*.pem"),
    ("x.key", "*.key"),
    ("x.p12", "*.p12"),
    ("id_rsa", "id_rsa*"),
    ("id_ed25519", "id_ed25519*"),
    (".DS_Store", ".DS_Store"),
    ("node_modules/x", "node_modules/"),
    ("target/x", "target/"),
    ("dist/x", "dist/"),
    ("build/x", "build/"),
    ("__pycache__/x", "__pycache__/"),
    (".venv/x", ".venv/"),
    ("venv/x", "venv/"),
    ("x.log", "*.log"),
    (".godterm-x", ".godterm-*"),
];

pub fn missing_ignores(ctx: &Ctx) -> Vec<String> {
    let mut a: Vec<String> = vec!["check-ignore".into(), "--no-index".into(), "--".into()];
    a.extend(WANTED.iter().map(|(p, _)| p.to_string()));
    let ignored: Vec<String> = match git_ops::run_any(ctx, &a, None) {
        Ok((_, out, _)) => out.lines().map(|l| l.trim().to_string()).collect(),
        Err(_) => return vec![],
    };
    WANTED
        .iter()
        .filter(|(p, _)| !ignored.iter().any(|i| i == p))
        .map(|(_, r)| r.to_string())
        .collect()
}

pub fn add_ignores(top: &Path, rules: &[String]) -> Result<String, String> {
    if rules.is_empty() {
        return Ok("Nothing to add.".into());
    }
    let p = top.join(".gitignore");
    if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(".gitignore is a symlink; edit it by hand".into());
    }
    let mut text = std::fs::read_to_string(&p).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("\n# Added by GodTerm (secrets and build output stay out of git)\n");
    for r in rules {
        if !WANTED.iter().any(|(_, w)| w == r) {
            return Err(format!("{r} is not a rule GodTerm adds"));
        }
        text.push_str(r);
        text.push('\n');
        if r == ".env.*" {
            text.push_str("!.env.example\n!.env.sample\n");
        }
    }
    std::fs::write(&p, text).map_err(|e| e.to_string())?;
    Ok(format!(
        "Added {} to .gitignore.",
        crate::control::plural(rules.len(), "rule")
    ))
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct PushCheck {
    pub secrets: SecretScan,
    pub sizes: Sizes,
    pub secret_files: Vec<String>,
    pub bad_links: Vec<(String, String)>,
    pub missing_ignores: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_error: Option<String>,

    pub head: Option<String>,
}

impl PushCheck {
    pub fn hard_blocks(&self) -> Vec<String> {
        let mut v = vec![];
        for (p, why) in &self.bad_links {
            v.push(format!(
                "{p} is a symlink that points at a private place: {why}"
            ));
        }
        for (p, n) in &self.sizes.block {
            v.push(format!(
                "{p} is {} (GitHub refuses files over 100 MB)",
                mb(*n)
            ));
        }
        if self.sizes.total > TOTAL_BYTES {
            v.push(format!(
                "the push is {} in all: split the push (over 1.5 GB)",
                mb(self.sizes.total)
            ));
        }
        if let Some(e) = &self.secrets.error {
            v.push(format!("the secret scan did not run: {e}"));
        }
        if let Some(e) = &self.size_error {
            v.push(format!("the size check did not run: {e}"));
        }
        v
    }

    pub fn soft_blocks(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .secrets
            .findings
            .iter()
            .map(|f| format!("possible secret: {}", f.line_text()))
            .collect();
        for f in &self.secret_files {
            v.push(format!("{f} is tracked and its name says it holds secrets"));
        }
        v
    }

    pub fn warnings(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .sizes
            .warn
            .iter()
            .map(|(p, n)| format!("{p} is {} (over 50 MB)", mb(*n)))
            .collect();
        if !self.missing_ignores.is_empty() {
            v.push(format!(
                ".gitignore does not cover: {}",
                self.missing_ignores.join(" ")
            ));
        }
        v
    }
}

pub fn check_push(ctx: &Ctx) -> PushCheck {
    let sizes = outgoing_sizes(ctx);
    PushCheck {
        secrets: scan_outgoing(ctx, gitleaks_bin().as_deref()),
        size_error: sizes.as_ref().err().cloned(),
        sizes: sizes.unwrap_or_default(),
        secret_files: tracked_secret_files(ctx),
        bad_links: bad_symlinks(ctx),
        missing_ignores: missing_ignores(ctx),
        head: git_ops::head(ctx),
    }
}

#[cfg(test)]
#[path = "git_scan_tests.rs"]
mod tests;
