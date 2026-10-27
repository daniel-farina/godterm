use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, SystemTime};

use super::pack;

pub const BUILD: &str = env!("GODTERM_WEB_BUILD");

static FILES: &[(&str, &[u8], &[u8])] = include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

pub const WATCH_EVERY: Duration = Duration::from_secs(1);

const STILL: u32 = 2;

pub struct Asset<'a> {
    pub mime: &'static str,
    pub bytes: &'a [u8],
    pub gz: &'a [u8],
}

type Bytes = (Cow<'static, [u8]>, Cow<'static, [u8]>);

pub struct Table {
    pub build: String,
    files: HashMap<String, Bytes>,
}

impl Table {
    pub fn embedded() -> Arc<Table> {
        static T: OnceLock<Arc<Table>> = OnceLock::new();
        T.get_or_init(|| {
            Arc::new(Table {
                build: BUILD.to_string(),
                files: FILES
                    .iter()
                    .map(|(p, b, z)| (p.to_string(), (Cow::Borrowed(*b), Cow::Borrowed(*z))))
                    .collect(),
            })
        })
        .clone()
    }

    pub fn from_pack(p: pack::Pack) -> Table {
        Table {
            build: p.id,
            files: p
                .files
                .into_iter()
                .map(|f| {
                    let gz = f.gz.unwrap_or_default();
                    (f.rel, (Cow::Owned(f.bytes), Cow::Owned(gz)))
                })
                .collect(),
        }
    }

    pub fn find(&self, path: &str) -> Option<Asset<'_>> {
        let (bytes, gz) = self.files.get(path)?;
        Some(Asset {
            mime: mime(path),
            bytes,
            gz,
        })
    }

    #[cfg(test)]
    pub fn paths(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.files.keys().map(|k| k.as_str()).collect();
        v.sort();
        v
    }
}

#[cfg(test)]

pub fn find(path: &str) -> Option<Asset<'static>> {
    let (_, bytes, gz) = FILES.iter().find(|(p, _, _)| *p == path)?;
    Some(Asset {
        mime: mime(path),
        bytes,
        gz,
    })
}

#[cfg(test)]
pub fn paths() -> Vec<&'static str> {
    FILES.iter().map(|f| f.0).collect()
}

type Fingerprint = Vec<(String, u64, Option<SystemTime>)>;

fn fingerprint(dir: &Path) -> Fingerprint {
    pack::walk(dir)
        .into_iter()
        .map(|(rel, p)| {
            let m = std::fs::metadata(&p).ok();
            let len = m.as_ref().map_or(0, |m| m.len());
            (rel, len, m.and_then(|m| m.modified().ok()))
        })
        .collect()
}

fn load(dir: &Path) -> Result<Table, String> {
    let p = pack::pack(dir).map_err(|e| e.to_string())?;
    if !p.files.iter().any(|f| f.rel == "index.html") {
        return Err(format!("{} has no index.html", dir.display()));
    }
    Ok(Table::from_pack(p))
}

pub struct Assets {
    current: RwLock<Arc<Table>>,

    pub dir: Option<PathBuf>,

    git: Option<PathBuf>,
    watch: std::sync::Mutex<Watch>,
}

#[derive(Default)]
struct Watch {
    loaded: Option<Fingerprint>,

    seen: Option<(Fingerprint, u32)>,

    failed: Option<String>,
}

impl Assets {
    pub fn embedded() -> Arc<Assets> {
        Arc::new(Assets {
            current: RwLock::new(Table::embedded()),
            dir: None,
            git: None,
            watch: Default::default(),
        })
    }

    pub fn from_dir(dir: PathBuf) -> Arc<Assets> {
        let fp = fingerprint(&dir);
        let (table, loaded) = match load(&dir) {
            Ok(t) => {
                crate::log::info(&format!(
                    "web: serving the app from {} (build {})",
                    dir.display(),
                    t.build
                ));
                (Arc::new(t), Some(fp))
            }
            Err(e) => {
                crate::log::error(&format!(
                    "web: the built in app until {} loads: {e}",
                    dir.display()
                ));
                (Table::embedded(), None)
            }
        };
        Arc::new(Assets {
            current: RwLock::new(table),
            git: git_dir(&dir),
            dir: Some(dir),
            watch: std::sync::Mutex::new(Watch {
                loaded,
                ..Default::default()
            }),
        })
    }

    pub fn current(&self) -> Arc<Table> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn build(&self) -> String {
        self.current().build.clone()
    }

    pub fn poll(&self) -> Option<String> {
        let dir = self.dir.as_ref()?;
        let fp = fingerprint(dir);
        let mut w = self.watch.lock().unwrap_or_else(|e| e.into_inner());
        if w.loaded.as_ref() == Some(&fp) {
            w.seen = None;
            return None;
        }
        match &mut w.seen {
            Some((seen, n)) if *seen == fp => {
                if self.git.as_deref().is_some_and(git_busy) {
                    return None;
                }
                *n += 1;
                if *n < STILL {
                    return None;
                }
            }
            _ => {
                w.seen = Some((fp, 0));
                return None;
            }
        }
        match load(dir) {
            Ok(t) => {
                w.loaded = Some(fp);
                w.seen = None;
                w.failed = None;
                let mut cur = self.current.write().unwrap_or_else(|e| e.into_inner());
                if cur.build == t.build {
                    return None;
                }
                let id = t.build.clone();
                *cur = Arc::new(t);
                crate::log::info(&format!("web: the app's files changed: build {id}"));
                Some(id)
            }
            Err(e) => {
                if w.failed.as_ref() != Some(&e) {
                    crate::log::error(&format!("web: kept the app as it was: {e}"));
                    w.failed = Some(e);
                }

                None
            }
        }
    }
}

fn git_dir(dir: &Path) -> Option<PathBuf> {
    let dir = crate::fspath::canonical(dir).ok()?;
    for a in dir.ancestors() {
        let g = a.join(".git");
        if g.is_dir() {
            return Some(g);
        }
        if let Ok(t) = std::fs::read_to_string(&g) {
            let p = t.trim().strip_prefix("gitdir:")?.trim();
            return Some(a.join(p));
        }
    }
    None
}

fn git_busy(git: &Path) -> bool {
    [
        "index.lock",
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ]
    .iter()
    .any(|f| git.join(f).exists())
}

pub fn dir_for(cfg: &crate::config::WebCfg) -> Option<PathBuf> {
    if !cfg.assets_dir.trim().is_empty() {
        return Some(crate::config::expand_tilde(cfg.assets_dir.trim()));
    }
    if cfg.embedded_assets || cfg!(test) {
        return None;
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let repo = repo.canonicalize().ok()?;
    let web = repo.join("web");
    (exe.starts_with(&repo) && web.join("index.html").is_file()).then_some(web)
}

pub fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "godterm-assets-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("js")).unwrap();
        d
    }

    fn write(d: &Path, rel: &str, text: &str) {
        std::fs::write(d.join(rel), text).unwrap();
    }

    fn app(tag: &str) -> PathBuf {
        let d = tmp(tag);
        write(
            &d,
            "index.html",
            "<script type=\"module\" src=\"js/main.js\"></script>",
        );
        write(&d, "js/main.js", "import { a } from './a.js';\n");
        write(&d, "js/a.js", "export const a = 1;\n");
        write(&d, "sw.js", "const BUILD = '__BUILD__';\n");
        d
    }

    #[test]
    fn a_folder_packs_like_build_rs() {
        let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
        let t = load(&web).unwrap();
        let e = Table::embedded();
        assert_eq!(t.build, BUILD);
        assert_eq!(t.paths(), paths());
        for p in paths() {
            let (a, b) = (t.find(p).unwrap(), find(p).unwrap());
            assert_eq!(a.bytes, b.bytes, "{p}");
            assert_eq!(a.gz, b.gz, "{p} gzipped");
            assert_eq!(e.find(p).unwrap().bytes, b.bytes);
        }
        assert!(t.find("dev/server.py").is_none());
    }

    #[test]
    fn versioning_names_the_build() {
        let d = app("ver");
        let t = load(&d).unwrap();
        let id = &t.build;
        let page = String::from_utf8(t.find("index.html").unwrap().bytes.to_vec()).unwrap();
        assert!(page.contains(&format!("js/main.js?v={id}")), "{page}");
        let m = String::from_utf8(t.find("js/main.js").unwrap().bytes.to_vec()).unwrap();
        assert!(m.contains(&format!("'./a.js?v={id}'")), "{m}");
        let sw = String::from_utf8(t.find("sw.js").unwrap().bytes.to_vec()).unwrap();
        assert!(sw.contains(&format!("'{id}'")), "{sw}");

        assert_eq!(load(&d).unwrap().build, *id);
        write(&d, "js/a.js", "export const a = 2;\n");
        assert_ne!(load(&d).unwrap().build, *id);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_watcher_swaps_in_a_change_once_it_holds_still() {
        let d = app("watch");
        let a = Assets::from_dir(d.clone());
        let first = a.build();
        assert_ne!(first, BUILD);
        let held = a.current();
        assert_eq!(a.poll(), None, "nothing changed");
        write(&d, "js/a.js", "export const a = 22;\n");
        assert_eq!(a.poll(), None, "a change is seen");
        assert_eq!(a.poll(), None, "and waits");
        let new = a.poll().expect("swapped in after two still checks");
        assert_ne!(new, first);
        assert_eq!(a.build(), new);
        assert_eq!(
            a.current().find("js/a.js").unwrap().bytes,
            b"export const a = 22;\n"
        );

        assert_eq!(held.build, first);
        assert_eq!(
            held.find("js/a.js").unwrap().bytes,
            b"export const a = 1;\n"
        );
        assert_eq!(a.poll(), None);

        write(&d, "js/a.js", "export const a = 3;\n");
        assert_eq!(a.poll(), None);
        assert_eq!(a.poll(), None);
        write(&d, "js/a.js", "export const a = 33;\n");
        assert_eq!(a.poll(), None);
        assert_eq!(a.poll(), None);
        assert!(a.poll().is_some());
        assert_eq!(
            a.current().find("js/a.js").unwrap().bytes,
            b"export const a = 33;\n"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_broken_folder_keeps_the_files_served() {
        let d = app("broken");
        let a = Assets::from_dir(d.clone());
        let good = a.build();

        std::fs::rename(d.join("index.html"), d.join("index.off")).unwrap();
        for _ in 0..3 {
            assert_eq!(a.poll(), None);
        }
        assert_eq!(a.build(), good);
        std::fs::rename(d.join("index.off"), d.join("index.html")).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            write(&d, "js/a.js", "export const a = 4;\n");
            let p = d.join("js/a.js");
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
            let unreadable = std::fs::read(&p).is_err();
            if unreadable {
                for _ in 0..3 {
                    assert_eq!(a.poll(), None);
                }
                assert_eq!(a.build(), good);
            }
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            if unreadable {
                assert!(a.poll().is_some(), "loaded once readable");
            } else {
                assert_eq!(a.poll(), None);
                assert_eq!(a.poll(), None);
                assert!(a.poll().is_some());
            }
            assert_eq!(
                a.current().find("js/a.js").unwrap().bytes,
                b"export const a = 4;\n"
            );
        }

        let none = Assets::from_dir(d.join("missing"));
        assert_eq!(none.build(), BUILD);
        assert_eq!(none.poll(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn nothing_loads_while_git_works() {
        let repo = tmp("git");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let d = repo.join("web");
        std::fs::rename(app("git-web"), &d).unwrap();
        let a = Assets::from_dir(d.clone());
        assert_eq!(
            a.git.as_deref(),
            Some(
                crate::fspath::canonical(&repo)
                    .unwrap()
                    .join(".git")
                    .as_path()
            )
        );
        let first = a.build();
        for busy in ["index.lock", "MERGE_HEAD", "rebase-merge"] {
            let b = repo.join(".git").join(busy);
            if busy.contains('-') {
                std::fs::create_dir_all(&b).unwrap();
            } else {
                std::fs::write(&b, "").unwrap();
            }
            write(&d, "js/a.js", &format!("export const a = '{busy}';\n"));
            for _ in 0..5 {
                assert_eq!(a.poll(), None, "{busy}");
            }
            assert_eq!(a.build(), first);
            let _ = std::fs::remove_dir_all(&b);
            let _ = std::fs::remove_file(&b);
            write(&d, "js/a.js", "export const a = 1;\n");

            let _ = a.poll();
        }

        write(&d, "js/a.js", "export const a = 5000;\n");
        assert_eq!(a.poll(), None);
        assert_eq!(a.poll(), None);
        assert!(a.poll().is_some(), "loads once git is done");

        let wt = tmp("git-wt");
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", repo.join(".git").display()),
        )
        .unwrap();
        assert_eq!(git_dir(&wt.join("js")), Some(repo.join(".git")));
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn nothing_outside_the_folder_is_served() {
        let d = app("outside");
        let out = tmp("outside-secret");
        write(&out, "secret.txt", "secret");
        write(&d, ".env", "hidden");
        std::fs::create_dir_all(d.join("dev")).unwrap();
        write(&d, "dev/server.py", "mock");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(out.join("secret.txt"), d.join("leak.txt")).unwrap();
            std::os::unix::fs::symlink(&out, d.join("leakdir")).unwrap();

            std::os::unix::fs::symlink(d.join("js/a.js"), d.join("alias.js")).unwrap();
            std::os::unix::fs::symlink(&d, d.join("js/loop")).unwrap();
        }
        let t = load(&d).unwrap();
        let paths = t.paths();
        for p in &paths {
            let bytes = t.find(p).unwrap().bytes;
            assert!(!bytes.starts_with(b"secret"), "{p} reaches outside");
        }
        for p in [
            "leak.txt",
            "leakdir/secret.txt",
            ".env",
            "dev/server.py",
            "../outside-secret/secret.txt",
            "/etc/passwd",
            "js/../index.html",
            "",
        ] {
            assert!(t.find(p).is_none(), "{p} {paths:?}");
        }
        #[cfg(unix)]
        assert!(t.find("alias.js").is_some(), "{paths:?}");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn the_folder_comes_from_config() {
        let mut c = crate::config::WebCfg::default();
        assert_eq!(dir_for(&c), None, "tests never pick the repo by themselves");
        c.assets_dir = "~/somewhere/web".into();
        let d = dir_for(&c).unwrap();
        assert!(d.ends_with("somewhere/web") && !d.starts_with("~"), "{d:?}");
        c.embedded_assets = true;
        assert!(dir_for(&c).is_some(), "an explicit folder wins");
    }

    #[test]
    fn an_older_build_is_not_cached_for_good() {
        let d = app("cache");
        let t = load(&d).unwrap();
        let ask = |q: &str| {
            let req = crate::web::http::parse(&format!(
                "GET /js/a.js{q} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
            ))
            .unwrap();
            let mut out = vec![];
            assert!(crate::web::http::serve_static(&mut out, &req, &t, &[]).unwrap());
            String::from_utf8_lossy(&out).into_owned()
        };
        let now = ask(&format!("?v={}", t.build));
        assert!(now.contains("immutable"), "{now}");
        assert!(now.contains(&format!("ETag: \"{}\"", t.build)), "{now}");
        let old = ask("?v=0123456789ab");
        assert!(old.contains("Cache-Control: no-cache"), "{old}");
        let none = ask("");
        assert!(none.contains("Cache-Control: no-cache"), "{none}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
