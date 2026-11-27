use anyhow::{bail, Context, Result};
use std::path::{Component, Path, PathBuf};

pub const POINTER: &str = "location";

pub const HINT: &str = "To move it: quit GodTerm, then run `godterm data-path <new folder>` (copies by default, --move moves). \
It writes a one line pointer file named `location` in the default folder (~/.godterm); GODTERM_HOME overrides it.";

const SKIP: &[&str] = &[
    "control.sock",
    "control.json",
    "godterm.lock",
    "godterm.lock.json",
    ".migrate.lock",
    POINTER,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Env,
    Pointer,
    Default,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Env => "GODTERM_HOME",
            Source::Pointer => "pointer file",
            Source::Default => "default",
        }
    }
}

pub fn default_root() -> PathBuf {
    crate::config::home_dir().join(".godterm")
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn absolute(raw: &str, base: Option<&Path>) -> PathBuf {
    let p = crate::config::expand_tilde(raw.trim());
    if p.is_absolute() {
        return normalize(&p);
    }
    let base = base
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(crate::config::home_dir);
    normalize(&base.join(p))
}

pub fn pointer_value(default: &Path) -> Option<String> {
    if let Ok(t) = std::fs::read_to_string(default.join(POINTER)) {
        if let Some(l) = t.lines().map(str::trim).find(|l| !l.is_empty()) {
            return Some(l.to_string());
        }
    }
    let t = std::fs::read_to_string(default.join("config.toml")).ok()?;
    let v: toml::Value = toml::from_str(&t).ok()?;
    v.get("data_dir")
        .and_then(|d| d.as_str())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
}

pub fn resolve_with(env: Option<&str>, pointer: Option<&str>, default: &Path) -> (PathBuf, Source) {
    if let Some(e) = env.filter(|e| !e.trim().is_empty()) {
        return (absolute(e, None), Source::Env);
    }
    if let Some(p) = pointer.filter(|p| !p.trim().is_empty()) {
        return (absolute(p, Some(default)), Source::Pointer);
    }
    (default.to_path_buf(), Source::Default)
}

static POINTER_CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

pub fn live() -> (PathBuf, Source) {
    let default = default_root();
    let env = crate::config::env_var("HOME");
    let ptr = if env.is_some() {
        None
    } else {
        POINTER_CACHE
            .get_or_init(|| pointer_value(&default))
            .clone()
    };
    let (p, s) = resolve_with(env.as_deref(), ptr.as_deref(), &default);
    if p.starts_with(crate::guard::VAULT) {
        return (default, Source::Default);
    }
    (p, s)
}

pub fn validate(p: &Path) -> Result<()> {
    use crate::platform::DirBuilderExt;
    if let Err(e) = crate::guard::resolve_outside_vault(p) {
        bail!("{e}");
    }
    if p.exists() && !p.is_dir() {
        bail!("{} is a file, not a folder", p.display());
    }
    if !p.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(p)
            .with_context(|| format!("creating {}", p.display()))?;
    }
    if !crate::platform::writable(p) {
        bail!("{} is not writable", p.display());
    }
    Ok(())
}

pub fn check() -> Result<()> {
    let default = default_root();
    let env = crate::config::env_var("HOME");
    let ptr = if env.is_some() {
        None
    } else {
        pointer_value(&default)
    };
    let (p, s) = resolve_with(env.as_deref(), ptr.as_deref(), &default);
    if p.starts_with(crate::guard::VAULT) {
        bail!(
            "the data path ({}) is in the private vault ({}); GodTerm never touches it. Fix {} or unset GODTERM_HOME",
            p.display(),
            crate::guard::VAULT,
            default.join(POINTER).display(),
        );
    }
    validate(&p).with_context(|| format!("the data path comes from the {}", s.label()))
}

pub fn running_in(root: &Path) -> bool {
    let lock = root.join("godterm.lock");
    if !lock.exists() {
        return false;
    }
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
    {
        Ok(f) => {
            let got = crate::platform::try_lock(&f);
            if got {
                crate::platform::unlock(&f);
            }
            !got
        }
        Err(_) => false,
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub files: u64,
    pub bytes: u64,
}

fn copy_tree(from: &Path, to: &Path, r: &mut Report) -> Result<()> {
    let md = std::fs::symlink_metadata(from)?;
    let ft = md.file_type();
    if ft.is_symlink() {
        let t = std::fs::read_link(from)?;
        let _ = std::fs::remove_file(to);
        crate::platform::symlink(&t, to)?;
        r.files += 1;
    } else if ft.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()), r)?;
        }
        let _ = std::fs::set_permissions(to, md.permissions());
    } else if ft.is_file() {
        r.bytes += std::fs::copy(from, to)?;
        r.files += 1;
    }
    Ok(())
}

fn is_empty_dir(p: &Path) -> bool {
    std::fs::read_dir(p)
        .map(|d| d.flatten().all(|e| e.file_name() == POINTER))
        .unwrap_or(true)
}

fn canon(p: &Path) -> PathBuf {
    crate::guard::canon(p)
}

pub fn migrate(src: &Path, dst_raw: &str, default: &Path, mv: bool) -> Result<(PathBuf, Report)> {
    let dst = absolute(dst_raw, None);
    if let Err(e) = crate::guard::resolve_outside_vault(&dst) {
        bail!("{e}");
    }
    if dst.exists() && !dst.is_dir() {
        bail!("{} is a file, not a folder", dst.display());
    }
    let (cs, cd) = (canon(src), canon(&dst));
    if cs == cd {
        bail!("{} is already the data path", dst.display());
    }
    if cd.starts_with(&cs) {
        bail!(
            "{} is inside the current data path {}",
            dst.display(),
            src.display()
        );
    }
    if cs.starts_with(&cd) {
        bail!(
            "the current data path {} is inside {}",
            src.display(),
            dst.display()
        );
    }
    if dst.exists() && !is_empty_dir(&dst) {
        bail!(
            "{} is not empty; pick an empty or new folder",
            dst.display()
        );
    }
    for r in [src, dst.as_path()] {
        if running_in(r) {
            bail!(
                "GodTerm is running (lock held in {}); quit it first",
                r.display()
            );
        }
    }
    validate(&dst)?;
    let mut rep = Report::default();
    if src.is_dir() {
        for e in std::fs::read_dir(src)? {
            let e = e?;
            let name = e.file_name();
            if SKIP.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            let (from, to) = (e.path(), dst.join(&name));
            if mv && std::fs::rename(&from, &to).is_ok() {
                rep.files += 1;
                continue;
            }
            copy_tree(&from, &to, &mut rep)
                .with_context(|| format!("copying {}", from.display()))?;
            if mv {
                let real_dir = std::fs::symlink_metadata(&from)?.file_type().is_dir();
                if real_dir {
                    std::fs::remove_dir_all(&from)?;
                } else {
                    std::fs::remove_file(&from)?;
                }
            }
        }
    }
    let ptr = default.join(POINTER);
    if canon(&dst) == canon(default) {
        let _ = std::fs::remove_file(&ptr);
    } else {
        std::fs::create_dir_all(default)?;
        crate::config::write_private(&ptr, format!("{}\n", dst.display()))?;
    }
    Ok((dst, rep))
}

pub fn command(args: &[String]) -> Result<()> {
    let (cur, source) = live();
    let target = args.iter().find(|a| !a.starts_with("--"));
    let mv = args.iter().any(|a| a == "--move");
    let Some(target) = target else {
        println!("data path   {}", cur.display());
        println!("chosen by   {}", source.label());
        println!("{HINT}");
        return Ok(());
    };
    if source == Source::Env {
        println!(
            "note: GODTERM_HOME is set and overrides the pointer file; unset it to use the new path"
        );
    }
    let (dst, rep) = migrate(&cur, target, &default_root(), mv)?;
    println!(
        "{} {} files ({} KB) from {} to {}",
        if mv { "moved" } else { "copied" },
        rep.files,
        rep.bytes.div_ceil(1024),
        cur.display(),
        dst.display()
    );
    println!("pointer     {}", default_root().join(POINTER).display());
    println!(
        "next        start GodTerm again; it now uses {}",
        dst.display()
    );
    if !mv {
        println!(
            "            the old copy in {} was left untouched; delete it once you are happy",
            cur.display()
        );
    }
    println!("            if you installed the supervisor, run `godterm supervisor install` again");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("godterm-dp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn full(p: &str) -> String {
        if cfg!(windows) {
            format!("C:{}", p.replace('/', "\\"))
        } else {
            p.to_string()
        }
    }

    #[test]
    fn resolution_order() {
        let d = PathBuf::from(full("/d/.godterm"));
        let (e, p) = (full("/e"), full("/p"));
        assert_eq!(resolve_with(None, None, &d), (d.clone(), Source::Default));
        assert_eq!(
            resolve_with(None, Some(&p), &d),
            (PathBuf::from(&p), Source::Pointer)
        );
        assert_eq!(
            resolve_with(Some(&e), Some(&p), &d),
            (PathBuf::from(&e), Source::Env)
        );
        assert_eq!(resolve_with(Some(" "), None, &d).1, Source::Default);
        assert_eq!(
            resolve_with(None, Some("rel/x"), &d).0,
            d.join("rel").join("x")
        );
    }

    #[test]
    fn tilde_and_dots() {
        let h = crate::config::home_dir();
        assert_eq!(absolute("~/a/../b", None), h.join("b"));
        assert_eq!(absolute("~", None), h);
        assert!(absolute("x", None).is_absolute());
    }

    #[test]
    fn pointer_file_and_config_key() {
        let d = tmp("ptr");
        assert_eq!(pointer_value(&d), None);
        std::fs::write(d.join("config.toml"), "data_dir = \"/from/config\"\n").unwrap();
        assert_eq!(pointer_value(&d).as_deref(), Some("/from/config"));
        std::fs::write(d.join(POINTER), "\n  /from/file  \n").unwrap();
        assert_eq!(pointer_value(&d).as_deref(), Some("/from/file"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn invalid_paths_are_refused() {
        let d = tmp("bad");
        let f = d.join("file");
        std::fs::write(&f, "x").unwrap();
        assert!(validate(&f).unwrap_err().to_string().contains("is a file"));
        assert!(validate(&f.join("sub")).is_err());
        let ok = d.join("new/deep");
        validate(&ok).unwrap();
        assert!(ok.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&ok).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn the_vault_is_refused() {
        let v = Path::new(crate::guard::VAULT).join("godterm");
        let e = validate(&v).unwrap_err().to_string();
        assert!(e.contains("private vault"), "{e}");
        let d = tmp("vault");
        let e = migrate(&d.join("src"), v.to_str().unwrap(), &d.join("def"), false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("private vault"), "{e}");
        assert!(!v.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn long_roots_get_a_short_socket() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = tmp("sock").join("a".repeat(120));
        std::fs::create_dir_all(&d).unwrap();
        crate::config::testing::set_home(&d);
        let p = crate::control::socket_path();
        assert!(p.as_os_str().len() < 104, "{}", p.display());
        assert!(!p.starts_with(&d));
        let _ = std::fs::remove_dir_all(d.parent().unwrap());
    }

    fn seed(src: &Path) {
        std::fs::create_dir_all(src.join("accounts/a")).unwrap();
        std::fs::write(src.join("config.toml"), "x = 1\n").unwrap();
        std::fs::write(src.join("accounts/a/f"), "hello").unwrap();
        std::fs::write(src.join("control.json"), "{}").unwrap();
        std::fs::write(src.join("godterm.lock"), "").unwrap();
    }

    #[test]
    fn copy_keeps_the_source() {
        let r = tmp("copy");
        let (src, def) = (r.join("src"), r.join("def"));
        seed(&src);
        let (dst, rep) = migrate(&src, r.join("out/new").to_str().unwrap(), &def, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.join("accounts/a/f")).unwrap(),
            "hello"
        );
        assert!(!dst.join("control.json").exists() && !dst.join("godterm.lock").exists());
        assert!(src.join("accounts/a/f").exists() && src.join("control.json").exists());
        assert_eq!(rep.files, 2);
        assert_eq!(pointer_value(&def).unwrap(), dst.display().to_string());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn move_removes_the_source_contents() {
        let r = tmp("move");
        let (src, def) = (r.join("src"), r.join("def"));
        seed(&src);
        let (dst, _) = migrate(&src, r.join("new").to_str().unwrap(), &def, true).unwrap();
        assert!(dst.join("accounts/a/f").exists() && dst.join("config.toml").exists());
        assert!(!src.join("accounts").exists() && !src.join("config.toml").exists());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn moving_back_to_the_default_clears_the_pointer() {
        let r = tmp("back");
        let (src, def) = (r.join("src"), r.join("def"));
        seed(&src);
        std::fs::create_dir_all(&def).unwrap();
        std::fs::write(def.join(POINTER), format!("{}\n", src.display())).unwrap();
        migrate(&src, def.to_str().unwrap(), &def, true).unwrap();
        assert!(!def.join(POINTER).exists());
        assert!(def.join("config.toml").exists());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn nonempty_nested_and_same_targets_are_refused() {
        let r = tmp("refuse");
        let (src, def) = (r.join("src"), r.join("def"));
        seed(&src);
        std::fs::create_dir_all(r.join("full")).unwrap();
        std::fs::write(r.join("full/x"), "").unwrap();
        for t in [r.join("full"), src.join("inner"), src.clone(), r.clone()] {
            assert!(
                migrate(&src, t.to_str().unwrap(), &def, false).is_err(),
                "{}",
                t.display()
            );
        }
        assert!(src.join("config.toml").exists());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_running_godterm_blocks_the_migration() {
        let r = tmp("running");
        let (src, def) = (r.join("src"), r.join("def"));
        seed(&src);
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(src.join("godterm.lock"))
            .unwrap();
        assert!(crate::platform::try_lock(&held));
        assert!(running_in(&src));
        let e = migrate(&src, r.join("new").to_str().unwrap(), &def, true)
            .unwrap_err()
            .to_string();
        assert!(e.contains("is running"), "{e}");
        assert!(!r.join("new").exists());
        crate::platform::unlock(&held);
        assert!(!running_in(&src));
        migrate(&src, r.join("new").to_str().unwrap(), &def, false).unwrap();
        let _ = std::fs::remove_dir_all(&r);
    }
}
