//! Canonical paths for the never list: every symlink on the way is
//! resolved, also one whose target is not there (an unmounted vault), so
//! a link into `/Volumes/Private` is caught whether or not it is mounted.

use std::path::{Component, Path, PathBuf};

/// `p` with every symlink resolved, component by component, as far as
/// links go (at most 40 hops); parts that do not exist are kept as named.
pub fn real(p: &Path) -> PathBuf {
    let mut hops = 0;
    resolve(p, &mut hops)
}

fn resolve(p: &Path, hops: &mut u32) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => {
                out.push(n);
                if *hops > 40 {
                    continue;
                }
                if let Ok(t) = std::fs::read_link(&out) {
                    *hops += 1;
                    out.pop();
                    let target = if t.is_absolute() { t } else { out.join(t) };
                    out = resolve(&target, hops);
                }
            }
        }
    }
    // /var -> /private/var and the like, for the part that exists (the
    // longest existing ancestor is canonical, the rest is kept as named).
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut base = out.clone();
    loop {
        if let Ok(c) = base.canonicalize() {
            let mut r = unverbatim(c);
            for t in tail.iter().rev() {
                r.push(t);
            }
            return r;
        }
        match (base.file_name().map(|n| n.to_os_string()), base.parent()) {
            (Some(n), Some(par)) => {
                tail.push(n);
                base = par.to_path_buf();
            }
            _ => return out,
        }
    }
}

/// `\\?\C:\x` (what canonicalize gives on Windows) as `C:\x`, and
/// `\\?\UNC\srv\share` as `\\srv\share`, so a canonical path and one
/// typed by a person compare equal. Other paths are returned as they are.
pub fn unverbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(r) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{r}"));
    }
    if let Some(r) = s.strip_prefix(r"\\?\") {
        let b = r.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return PathBuf::from(r);
        }
    }
    p
}

/// Whether `p` is `d` or inside it, comparing component by component and,
/// on Windows, ignoring case (and `/` against `\`).
pub fn under(p: &Path, d: &Path) -> bool {
    if !cfg!(windows) {
        return p.starts_with(d);
    }
    let norm = |x: &Path| -> Vec<String> {
        x.to_string_lossy()
            .replace('/', "\\")
            .split('\\')
            .filter(|c| !c.is_empty())
            .map(|c| c.to_lowercase())
            .collect()
    };
    let (a, b) = (norm(p), norm(d));
    a.len() >= b.len() && a[..b.len()] == b[..]
}

/// Why `p` may not be synced, if it is inside (or links into) one of
/// `never` (canonical folders).
pub fn refused(p: &Path, never: &[PathBuf]) -> Option<String> {
    let r = real(p);
    for d in never {
        if under(&r, d) || under(p, d) {
            return Some(format!("{} is inside {}", p.display(), d.display()));
        }
    }
    if under(p, Path::new(super::VAULT)) || under(&r, Path::new(super::VAULT)) {
        return Some(format!("{} is in the vault", p.display()));
    }
    None
}

/// A temp folder cwd: sessions there are left out.
pub fn is_temp(cwd: &Path) -> bool {
    let s = cwd.to_string_lossy();
    [
        "/private/var/folders",
        "/var/folders",
        "/tmp",
        "/private/tmp",
    ]
    .iter()
    .any(|t| s == *t || s.starts_with(&format!("{t}/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefixes_come_off_and_windows_paths_compare_by_component() {
        assert_eq!(
            unverbatim(PathBuf::from(r"\\?\C:\Users\x")),
            PathBuf::from(r"C:\Users\x")
        );
        assert_eq!(
            unverbatim(PathBuf::from(r"\\?\UNC\srv\sh\x")),
            PathBuf::from(r"\\srv\sh\x")
        );
        assert_eq!(unverbatim(PathBuf::from("/a/b")), PathBuf::from("/a/b"));
        assert!(under(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(!under(Path::new("/a/bc"), Path::new("/a/b")));
        if cfg!(windows) {
            assert!(under(
                Path::new(r"c:\USERS\x\Vault\p"),
                Path::new(r"C:\Users\X\vault")
            ));
            assert!(under(
                Path::new("C:/Users/x/vault/p"),
                Path::new(r"C:\Users\x\vault")
            ));
            assert!(!under(
                Path::new(r"C:\Users\x\vault2"),
                Path::new(r"C:\Users\x\vault")
            ));
        }
    }

    #[test]
    fn dangling_links_into_the_vault_are_caught() {
        let d = std::env::temp_dir().join(format!("gt-sync-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/Volumes/Private/secret", d.join("a/v")).unwrap();
            std::os::unix::fs::symlink("a", d.join("b")).unwrap();
            assert_eq!(
                real(&d.join("a/v/x")),
                PathBuf::from("/Volumes/Private/secret/x")
            );
            assert!(refused(&d.join("b/v/notes.md"), &[]).is_some());
            assert!(refused(&d.join("a"), &[]).is_none());
            assert_eq!(real(&d.join("b")), real(&d.join("a")));
        }
        assert!(refused(Path::new("/Volumes/Private/x"), &[]).is_some());
        assert!(is_temp(Path::new("/private/var/folders/xy/T/abc")));
        assert!(is_temp(Path::new("/tmp")));
        assert!(!is_temp(Path::new("/tmpfoo")));
        let _ = std::fs::remove_dir_all(&d);
    }
}
