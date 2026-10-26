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
    // /var -> /private/var and the like, for the part that exists.
    if let Ok(c) = out.canonicalize() {
        return c;
    }
    out
}

/// Why `p` may not be synced, if it is inside (or links into) one of
/// `never` (canonical folders).
pub fn refused(p: &Path, never: &[PathBuf]) -> Option<String> {
    let r = real(p);
    let raw = p.to_string_lossy();
    for d in never {
        if r.starts_with(d) || raw.starts_with(&*d.to_string_lossy()) {
            return Some(format!("{} is inside {}", p.display(), d.display()));
        }
    }
    if raw.starts_with(super::VAULT) || r.starts_with(super::VAULT) {
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
