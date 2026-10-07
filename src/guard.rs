//! What the assistant's file tools may never touch, whatever folder a tab
//! is in: GodTerm's own home (account logins, the control token), the
//! Claude Code and Grok installs, keys and credentials. And which folders
//! a tab may be opened in at all.

use std::path::{Path, PathBuf};

/// The real path: symlinks resolved for the part that exists (so a path
/// that is not there yet still compares right, /var vs /private/var).
fn canon(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    let mut rest = vec![];
    let mut cur = p.to_path_buf();
    while let Some(name) = cur.file_name().map(|n| n.to_os_string()) {
        rest.push(name);
        if !cur.pop() {
            break;
        }
        if let Ok(c) = cur.canonicalize() {
            let mut out = c;
            for n in rest.iter().rev() {
                out.push(n);
            }
            return out;
        }
    }
    p.to_path_buf()
}

/// Folders whose contents are private, wherever a tab is.
fn private_dirs() -> Vec<PathBuf> {
    let h = crate::config::home_dir();
    let mut v: Vec<PathBuf> = [
        ".claude",
        ".claudego",
        ".grok",
        ".godterm",
        ".ssh",
        ".aws",
        ".gnupg",
        ".config/gh",
        ".config/gcloud",
        ".kube",
        ".docker",
        ".azure",
        ".password-store",
        "Library/Keychains",
        "Library/Application Support/Claude",
        "Library/Cookies",
    ]
    .iter()
    .map(|d| h.join(d))
    .collect();
    v.push(crate::config::app_home());
    v.push(crate::config::dirs().main_claude);
    v.push(crate::config::dirs().main_grok);
    v.push(PathBuf::from("/Library/Keychains"));
    v.push(PathBuf::from("/private/var/db"));
    v.into_iter().map(|p| canon(&p)).collect()
}

/// A path as the OS compares them: on Windows without the \\?\ prefix
/// canonicalize adds, one separator, and case-insensitive.
fn path_key(p: &str, windows: bool) -> String {
    if !windows {
        return p.trim_end_matches('/').to_string();
    }
    let s = p.strip_prefix(r"\\?\").unwrap_or(p).replace('/', "\\");
    s.trim_end_matches('\\').to_lowercase()
}

/// `child` is strictly inside `parent` (an ancestor, not the same folder).
pub fn strictly_inside(child: &str, parent: &str, windows: bool) -> bool {
    let (c, p) = (path_key(child, windows), path_key(parent, windows));
    let sep = if windows { '\\' } else { '/' };
    c.len() > p.len() && c.starts_with(&p) && (p.ends_with(sep) || c[p.len()..].starts_with(sep))
}

fn inside(child: &Path, parent: &Path) -> bool {
    strictly_inside(
        &child.to_string_lossy(),
        &parent.to_string_lossy(),
        cfg!(windows),
    )
}

fn same(a: &Path, b: &Path) -> bool {
    path_key(&a.to_string_lossy(), cfg!(windows)) == path_key(&b.to_string_lossy(), cfg!(windows))
}

/// A file name that holds secrets.
pub fn secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let stem = n.split('.').next().unwrap_or("");
    n.contains("credentials")
        || n == "auth.json"
        || n == ".netrc"
        || n == ".pgpass"
        || n == ".npmrc"
        || n == ".pypirc"
        || n == "control.json"
        || n.starts_with("id_rsa")
        || n.starts_with("id_ed25519")
        || n.starts_with("id_ecdsa")
        || n.ends_with(".pem")
        || n.ends_with(".key")
        || n.ends_with(".p12")
        || n.ends_with(".keychain")
        || n.ends_with(".keychain-db")
        || n.ends_with(".token")
        || matches!(
            stem,
            "token" | "tokens" | "access_token" | "refresh_token" | "secrets" | ".claude"
        )
}

/// Why `p` may not be read, listed or opened by the assistant, if so.
/// `allowed` are folders inside a private one that the user made tabs in
/// (tab workspaces under GodTerm's home).
pub fn denied(p: &Path, allowed: &[PathBuf]) -> Option<String> {
    let c = canon(p);
    if let Some(name) = c.file_name().map(|n| n.to_string_lossy().into_owned()) {
        if secret_name(&name) {
            return Some(format!("{name} holds credentials or keys"));
        }
    }
    let ok_here = allowed.iter().any(|a| {
        let a = canon(a);
        c.starts_with(&a)
            && !private_dirs()
                .iter()
                .any(|d| d.starts_with(&a) && c.starts_with(d))
    });
    for d in private_dirs() {
        if c.starts_with(&d) && !ok_here {
            return Some(format!(
                "{} is private (logins, tokens or keys live there)",
                crate::config::tilde(&d)
            ));
        }
    }
    None
}

/// Document types open_path may hand to their default app.
const DOCS: &[&str] = &[
    "html", "htm", "pdf", "png", "jpg", "jpeg", "gif", "svg", "webp", "heic", "bmp", "tiff", "ico",
    "md", "markdown", "txt", "rtf", "json", "csv", "tsv", "log", "xml", "yaml", "yml", "toml",
    "mp4", "mov", "m4v", "webm", "mp3", "m4a", "wav", "aac", "flac", "ogg", "docx", "xlsx", "pptx",
    "pages", "numbers", "key", "odt", "ods", "epub",
];

/// Why open_path must not open `p` (it could run code), if so. Folders
/// open in Finder unless they are bundles; files only as documents.
pub fn open_refusal(p: &Path) -> Option<String> {
    use crate::platform::PermissionsExt;
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let md = std::fs::metadata(p).ok()?;
    if md.is_dir() {
        let bundles = [
            "app",
            "bundle",
            "framework",
            "workflow",
            "pkg",
            "mpkg",
            "prefpane",
            "action",
            "plugin",
            "kext",
            "service",
            "saver",
            "qlgenerator",
            "xpc",
        ];
        return bundles
            .contains(&ext.as_str())
            .then(|| format!(".{ext} is an app or installer bundle; it would run"));
    }
    // ".key" is Keynote here; private keys were refused by `denied` first.
    if !DOCS.contains(&ext.as_str()) {
        return Some(if ext.is_empty() {
            "a file with no document type could run as a program".into()
        } else {
            format!(".{ext} is not a document type (it could run code)")
        });
    }
    if md.permissions().mode() & 0o111 != 0 {
        return Some("it is marked executable".into());
    }
    None
}

/// Windows system folders: a drive root, the Users folder itself, and the
/// Windows, Program Files and ProgramData trees.
#[cfg(windows)]
fn windows_system(c: &Path) -> bool {
    use std::path::Component;
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from).map(|p| canon(&p));
    let normal = c
        .components()
        .filter(|x| matches!(x, Component::Normal(_)))
        .count();
    if normal == 0 {
        return true;
    }
    // (SystemDrive is "C:", which alone means C's current folder.)
    let users = std::env::var("SystemDrive")
        .ok()
        .map(|d| canon(Path::new(&format!("{d}\\Users"))));
    if users.is_some_and(|u| *c == u) {
        return true;
    }
    [
        "SystemRoot",
        "windir",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "ProgramData",
    ]
    .iter()
    .filter_map(|k| env(k))
    .any(|t| c.starts_with(&t))
}

#[cfg(not(windows))]
fn windows_system(_c: &Path) -> bool {
    false
}

/// A system folder on this OS that exists (for tests and examples).
#[cfg(test)]
pub fn a_system_folder() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
    } else {
        PathBuf::from("/etc")
    }
}

/// Where a tab may be opened: Ok(false) fine, Ok(true) needs the user's
/// yes (the home folder itself), Err refused.
pub fn tab_folder(p: &Path, allowed: &[PathBuf]) -> Result<bool, String> {
    let c = canon(p);
    let home = canon(&crate::config::home_dir());
    // The temp folders (on Windows the one in the profile's AppData).
    let sys_tmp = canon(&std::env::temp_dir());
    let tmp = |c: &Path| {
        c.starts_with("/private/var/folders")
            || c.starts_with("/private/tmp")
            || c.starts_with("/tmp")
            || same(c, &sys_tmp)
            || inside(c, &sys_tmp)
    };
    // Roots that hold everything, and system trees.
    let exact = [
        "/",
        "/Users",
        "/Volumes",
        "/private",
        "/Applications",
        "/opt",
        "/Library",
    ];
    let trees = [
        "/System",
        "/usr",
        "/bin",
        "/sbin",
        "/etc",
        "/private/etc",
        "/var",
        "/private/var",
        "/Library",
        "/dev",
        "/cores",
    ];
    let system = exact.iter().any(|s| c == Path::new(s))
        || (!tmp(&c) && !c.starts_with(&home) && trees.iter().any(|s| c.starts_with(s)))
        || windows_system(&c);
    if system {
        return Err(format!(
            "{} is a system folder; tabs are opened in project folders",
            c.display()
        ));
    }
    if let Some(why) = denied(&c, allowed) {
        return Err(why);
    }
    // A parent of a private folder would bring it into the tab's scope.
    if !same(&c, &home) && private_dirs().iter().any(|d| inside(d, &c)) && !tmp(&c) {
        return Err(format!(
            "{} contains private folders (logins and keys)",
            crate::config::tilde(&c)
        ));
    }
    Ok(c == home)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows shaped paths, checked on every OS (pure path logic).
    #[test]
    fn ancestors_the_windows_way() {
        let w = true;
        let home = r"C:\Users\Nev";
        assert!(strictly_inside(r"\\?\C:\Users\Nev\.claude", home, w));
        assert!(strictly_inside(
            r"c:\users\nev\.claude",
            r"\\?\C:\USERS\NEV",
            w
        ));
        assert!(!strictly_inside(
            r"C:\Users\Nev\.claude",
            r"C:\Users\Nev\AppData\Local\Temp",
            w
        ));
        assert!(!strictly_inside(home, home, w), "not its own ancestor");
        assert!(!strictly_inside(r"C:\Users\Nevx\.ssh", home, w));
        assert!(strictly_inside(r"C:\Users\Nev\.ssh", r"C:\", w));
        assert!(strictly_inside("/u/x/.ssh", "/u/x", false));
        assert!(!strictly_inside("/u/xy", "/u/x", false));
        assert!(strictly_inside("/u/x", "/", false));
    }

    #[test]
    fn denylist() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let h = crate::config::home_dir();
        for p in [
            h.join(".ssh/id_ed25519"),
            h.join(".claude/.credentials.json"),
            h.join(".grok/auth.json"),
            h.join(".godterm/control.json"),
            h.join(".aws/credentials"),
            h.join("Library/Keychains/login.keychain-db"),
        ] {
            assert!(denied(&p, &[]).is_some(), "{}", p.display());
        }
        let home = crate::config::app_home();
        assert!(denied(&home.join("accounts/one/.credentials.json"), &[]).is_some());
        assert!(denied(&home.join("control.json"), &[]).is_some());
        // Any folder: secret file names are refused.
        let tmp = std::env::temp_dir().join("proj");
        for n in [
            "auth.json",
            "x.pem",
            "server.key",
            "token",
            "api.token",
            ".netrc",
            "aws_credentials",
        ] {
            assert!(denied(&tmp.join(n), &[]).is_some(), "{n}");
        }
        for n in ["main.rs", "tokenizer.rs", "README.md", "keys.md"] {
            assert!(denied(&tmp.join(n), &[]).is_none(), "{n}");
        }
        // A workspace the user made under GodTerm's home is fine, its
        // logins are not.
        let ws = home.join("tabs/demo");
        assert!(denied(&ws.join("a.txt"), std::slice::from_ref(&ws)).is_none());
        assert!(denied(
            &home.join("accounts/x/settings.json"),
            std::slice::from_ref(&ws)
        )
        .is_some());
    }

    #[test]
    fn open_only_documents() {
        use crate::platform::PermissionsExt;
        let d = std::env::temp_dir().join(format!("godterm-open-{}", std::process::id()));
        std::fs::create_dir_all(d.join("Evil.app/Contents")).unwrap();
        std::fs::create_dir_all(d.join("site")).unwrap();
        for f in [
            "README.command",
            "x.terminal",
            "run.sh",
            "tool",
            "index.html",
            "notes.md",
            "script.html",
        ] {
            std::fs::write(d.join(f), "x").unwrap();
        }
        std::fs::set_permissions(
            d.join("script.html"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::create_dir_all(d.join("run.workflow")).unwrap();
        for bad in [
            "README.command",
            "x.terminal",
            "run.sh",
            "tool",
            "Evil.app",
            "run.workflow",
        ] {
            assert!(open_refusal(&d.join(bad)).is_some(), "{bad}");
        }
        // The executable bit exists on Unix only.
        if cfg!(unix) {
            assert!(open_refusal(&d.join("script.html")).is_some());
        }
        for good in ["index.html", "notes.md", "site"] {
            assert!(open_refusal(&d.join(good)).is_none(), "{good}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn tab_folders() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let h = crate::config::home_dir();
        let system: Vec<PathBuf> = if cfg!(windows) {
            let e = |k: &str| std::env::var_os(k).map(PathBuf::from);
            let drive = e("SystemDrive").unwrap_or_else(|| "C:".into());
            [
                Some(drive.join("\\")),
                Some(drive.join("\\").join("Users")),
                e("SystemRoot"),
                e("ProgramFiles").map(|p| p.join("Common Files")),
                e("ProgramData"),
            ]
            .into_iter()
            .flatten()
            .collect()
        } else {
            [
                "/",
                "/etc",
                "/usr/local",
                "/System/Library",
                "/private/etc",
                "/Users",
            ]
            .iter()
            .map(PathBuf::from)
            .collect()
        };
        for p in &system {
            assert!(tab_folder(p, &[]).is_err(), "{}", p.display());
        }
        assert!(tab_folder(&h.join(".ssh"), &[]).is_err());
        assert!(tab_folder(&crate::config::app_home(), &[]).is_err());
        assert_eq!(tab_folder(&h, &[]), Ok(true), "home itself needs a yes");
        assert_eq!(tab_folder(&std::env::temp_dir(), &[]), Ok(false));
    }
}
