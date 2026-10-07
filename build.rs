//! Build metadata for `godterm --version`: the git commit and the build date.
//! Set GODTERM_GIT_SHA or SOURCE_DATE_EPOCH to override (reproducible builds,
//! tarballs without .git).

use std::process::Command;

fn main() {
    let sha = std::env::var("GODTERM_GIT_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let out = Command::new("git")
                .args(["rev-parse", "--short=9", "HEAD"])
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        });
    let sha = sha.unwrap_or_else(|| "unknown".into());
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });
    println!("cargo:rustc-env=GODTERM_GIT_SHA={sha}");
    println!("cargo:rustc-env=GODTERM_BUILD_DATE={}", civil_date(secs));
    println!(
        "cargo:rustc-env=GODTERM_TARGET={}",
        std::env::var("TARGET").unwrap_or_default()
    );
    println!("cargo:rerun-if-env-changed=GODTERM_GIT_SHA");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    // Rerun when HEAD moves: the HEAD file itself and the branch it names.
    if let Ok(head) = std::fs::read_to_string(".git/HEAD") {
        println!("cargo:rerun-if-changed=.git/HEAD");
        if let Some(r) = head.trim().strip_prefix("ref: ") {
            if std::path::Path::new(".git").join(r).exists() {
                println!("cargo:rerun-if-changed=.git/{r}");
            }
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
    windows_resources();
}

/// The icon and version info in godterm.exe (Windows targets, built on Windows).
#[cfg(windows)]
fn windows_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=packaging/windows/godterm.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("packaging/windows/godterm.ico")
        .set("ProductName", "GodTerm")
        .set("FileDescription", "GodTerm")
        .set("CompanyName", "Daniel Farina")
        .set(
            "LegalCopyright",
            "Copyright 2026 Daniel Farina. MIT License.",
        );
    if let Err(e) = res.compile() {
        println!("cargo:warning=godterm.exe built without its icon: {e}");
    }
}

#[cfg(not(windows))]
fn windows_resources() {}

/// UTC "YYYY-MM-DD" for unix seconds (days-to-civil, no dependencies).
fn civil_date(secs: i64) -> String {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}
