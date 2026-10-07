//! Folder trust. claude asks "Accessing workspace: ... Yes, I trust this
//! folder" until `projects["<dir>"].hasTrustDialogAccepted` is true in the
//! slot's `.claude.json`. claude 2.1.29x never persists that answer for the
//! home directory ("home trust is session-only"), so tabs started in `~`
//! asked on every launch. With `auto_trust` godterm seeds the flag before
//! starting claude and answers the dialog if it still appears.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::expand_tilde;

/// The keys claude may look the folder up by: the path as given and its
/// canonical form (symlinks resolved, no trailing slash).
pub fn trust_keys(cwd: &Path) -> Vec<String> {
    // As claude writes them: no trailing separator, and on Windows no
    // \\?\ prefix (what canonicalize adds).
    let key = |p: &Path| {
        let s = p.to_string_lossy();
        let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
        let t = s.trim_end_matches(['/', '\\']);
        if t.ends_with(':') {
            s.to_string()
        } else {
            t.to_string()
        }
    };
    let mut v = vec![];
    let plain = key(cwd);
    if !plain.is_empty() {
        v.push(plain);
    }
    if let Ok(c) = fs::canonicalize(cwd) {
        let c = key(&c);
        if !v.contains(&c) {
            v.push(c);
        }
    }
    v
}

/// Whether `cwd` may be trusted automatically: always when `trusted_dirs`
/// is empty, otherwise only inside one of them.
pub fn dir_allowed(cwd: &Path, trusted_dirs: &[String]) -> bool {
    if trusted_dirs.is_empty() {
        return true;
    }
    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let c = canon(cwd);
    trusted_dirs.iter().any(|d| {
        let root: PathBuf = canon(&expand_tilde(d));
        c.starts_with(&root)
    })
}

/// Mark `cwd` trusted in `<config_dir>/.claude.json`, keeping every other
/// key. Written through a temp file and rename so a reader never sees half
/// a file. Returns true when the file changed.
pub fn seed_trust(config_dir: &Path, cwd: &Path) -> std::io::Result<bool> {
    let path = config_dir.join(".claude.json");
    let mut root: Value = match fs::read_to_string(&path) {
        Ok(t) => match serde_json::from_str(&t) {
            Ok(v) => v,
            // Never overwrite a file we cannot parse.
            Err(_) => return Ok(false),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(Default::default()),
        Err(e) => return Err(e),
    };
    let Some(obj) = root.as_object_mut() else {
        return Ok(false);
    };
    let projects = obj
        .entry("projects")
        .or_insert_with(|| Value::Object(Default::default()));
    let Some(projects) = projects.as_object_mut() else {
        return Ok(false);
    };
    let mut changed = false;
    for key in trust_keys(cwd) {
        let entry = projects
            .entry(key)
            .or_insert_with(|| Value::Object(Default::default()));
        if let Some(e) = entry.as_object_mut() {
            if e.get("hasTrustDialogAccepted").and_then(Value::as_bool) != Some(true) {
                e.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(false);
    }
    fs::create_dir_all(config_dir)?;
    let tmp = config_dir.join(format!(".claude.json.godterm-{}", std::process::id()));
    fs::write(&tmp, serde_json::to_vec_pretty(&root).unwrap_or_default())?;
    #[cfg(unix)]
    {
        use crate::platform::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, &path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("godterm-trust-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn seeds_and_keeps_other_keys() {
        let cfg = tmp("seed");
        let work = cfg.join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(
            cfg.join(".claude.json"),
            r#"{"theme":"dark","oauthAccount":{"emailAddress":"a@b.c"},"projects":{"/other":{"allowedTools":["Bash"],"hasTrustDialogAccepted":false}}}"#,
        )
        .unwrap();
        assert!(seed_trust(&cfg, &work).unwrap());
        let v: Value =
            serde_json::from_str(&fs::read_to_string(cfg.join(".claude.json")).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["oauthAccount"]["emailAddress"], "a@b.c");
        assert_eq!(v["projects"]["/other"]["allowedTools"][0], "Bash");
        for k in trust_keys(&work) {
            assert_eq!(v["projects"][&k]["hasTrustDialogAccepted"], true, "{k}");
        }
        // Second time: nothing to do.
        assert!(!seed_trust(&cfg, &work).unwrap());
        // Missing file: created. Unparsable file: left alone.
        let cfg2 = tmp("missing");
        assert!(seed_trust(&cfg2, &work).unwrap());
        fs::write(cfg2.join(".claude.json"), "{not json").unwrap();
        assert!(!seed_trust(&cfg2, &cfg).unwrap());
        assert_eq!(
            fs::read_to_string(cfg2.join(".claude.json")).unwrap(),
            "{not json"
        );
        let _ = fs::remove_dir_all(&cfg);
        let _ = fs::remove_dir_all(&cfg2);
    }

    #[test]
    fn keys_and_allow_list() {
        let d = tmp("keys");
        let link = d.join("link");
        let real = d.join("real");
        fs::create_dir_all(&real).unwrap();
        #[cfg(unix)]
        crate::platform::symlink(&real, &link).unwrap();
        let keys = trust_keys(&link);
        let sep = std::path::MAIN_SEPARATOR;
        assert!(keys.iter().any(|k| k.ends_with(&format!("{sep}link"))));
        if cfg!(unix) {
            // (Where the link resolves to: Unix makes the symlink.)
            assert!(keys.iter().any(|k| k.ends_with(&format!("{sep}real"))));
        }
        assert!(keys.iter().all(|k| !k.starts_with(r"\\?\")));
        assert!(trust_keys(Path::new("/tmp/x/"))
            .iter()
            .all(|k| !k.ends_with('/')));
        assert!(dir_allowed(&real, &[]));
        assert!(dir_allowed(&real, &[d.to_string_lossy().into_owned()]));
        assert!(!dir_allowed(&real, &["/definitely/elsewhere".into()]));
        let _ = fs::remove_dir_all(&d);
    }
}
