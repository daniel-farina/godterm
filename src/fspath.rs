//! Paths handed to other programs. Windows `canonicalize` returns the
//! verbatim form (`\\?\C:\x`, `\\?\UNC\srv\share`), which git and most
//! tools do not understand inside URLs or arguments; this turns it back
//! into the ordinary spelling. Elsewhere it changes nothing.

use std::path::{Path, PathBuf};

/// `p` without a verbatim prefix (`\\?\C:\x` becomes `C:\x`,
/// `\\?\UNC\srv\share` becomes `\\srv\share`). Other paths are returned as is.
pub fn plain(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match strip(&s) {
        Some(r) => PathBuf::from(r),
        None => p.to_path_buf(),
    }
}

fn strip(s: &str) -> Option<String> {
    if let Some(r) = s.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{r}"));
    }
    let r = s.strip_prefix(r"\\?\")?;
    // Only a drive path is safe to unwrap (\\?\Volume{..}\ stays).
    let b = r.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':').then(|| r.to_string())
}

/// `std::fs::canonicalize` without the verbatim prefix.
pub fn canonical(p: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(p).map(|c| plain(&c))
}

/// `file://` URL of a local folder that git accepts on every OS
/// (`file:///C:/x/y` on Windows).
#[cfg(test)]
pub fn file_url(p: &Path) -> String {
    let s = plain(p).to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// A remote or path that names a drive path (`C:\x`, `C:/x`), which looks
/// like an scp style `host:path` but is a folder.
pub fn is_drive_path(u: &str) -> bool {
    let b = u.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefix_is_removed() {
        let p = |s: &str| plain(Path::new(s));
        assert_eq!(p(r"\\?\C:\Users\x\r.git"), PathBuf::from(r"C:\Users\x\r.git"));
        assert_eq!(p(r"\\?\UNC\srv\share\r"), PathBuf::from(r"\\srv\share\r"));
        assert_eq!(p(r"\\?\Volume{1}\r"), PathBuf::from(r"\\?\Volume{1}\r"));
        assert_eq!(p("/tmp/x"), PathBuf::from("/tmp/x"));
        assert_eq!(p(r"C:\a"), PathBuf::from(r"C:\a"));
    }

    #[test]
    fn urls_and_drive_paths() {
        assert_eq!(file_url(Path::new("/tmp/a b")), "file:///tmp/a b");
        assert_eq!(
            file_url(Path::new(r"\\?\C:\Users\x\remote.git")),
            "file:///C:/Users/x/remote.git"
        );
        assert!(is_drive_path(r"C:\x") && is_drive_path("d:/x"));
        assert!(!is_drive_path("git@host:x") && !is_drive_path("C:x"));
    }
}
