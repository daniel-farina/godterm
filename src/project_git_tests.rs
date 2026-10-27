//! Git facts on real temp repos: a bare remote as a local path, a
//! GitHub looking remote (never contacted), ahead and behind through a
//! second clone, dirty files and stashes, credentials in URLs, a repo
//! with no remote, a plain folder and a folder of repos. And that reading
//! never writes into a repo.

use super::*;
use crate::config::testing::LOCK as ENV_LOCK;
use serde_json::json;

fn base(tag: &str) -> PathBuf {
    let b = std::env::temp_dir().join(format!("godterm-git-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&b);
    std::fs::create_dir_all(&b).unwrap();
    // Canonical: /var is /private/var on macOS, and git says the latter.
    crate::fspath::canonical(&b).unwrap()
}

/// Run git for the fixture (isolated from the user's own settings).
fn g(dir: &Path, args: &[&str]) -> String {
    let home = std::env::temp_dir().join(format!("godterm-git-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Test Person",
            "-c",
            "user.email=t@example.com",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env("HOME", &home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn w(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn commit(dir: &Path, file: &str, text: &str, msg: &str) {
    w(&dir.join(file), text);
    g(dir, &["add", file]);
    g(dir, &["commit", "-q", "-m", msg]);
}

fn facts(dir: &Path) -> Facts {
    let (t, gd, c) = locate(dir).expect("a repo");
    compute(&t, &gd, &c, Duration::from_secs(5))
}

struct Fx {
    root: PathBuf,
    bare: PathBuf,
    /// Pushed to the bare remote, then one more local commit.
    a: PathBuf,
    /// A second clone that pushed a commit A has not fetched.
    b: PathBuf,
    /// Remote URL on GitHub with a token in it; refs as if fetched.
    gh: PathBuf,
    /// No remote at all.
    lonely: PathBuf,
    /// Not in git.
    plain: PathBuf,
    /// A folder of repos (not one itself) with a repo inside.
    shelf: PathBuf,
}

fn fixture(tag: &str) -> Fx {
    let root = base(tag);
    let bare = root.join("remote.git");
    std::fs::create_dir_all(&bare).unwrap();
    g(&bare, &["init", "-q", "--bare"]);
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    g(&seed, &["init", "-q"]);
    commit(&seed, "README.md", "# seed\n", "first");
    g(&seed, &["remote", "add", "origin", bare.to_str().unwrap()]);
    g(&seed, &["push", "-q", "origin", "main"]);
    let a = root.join("alpha");
    let b = root.join("beta");
    g(&root, &["clone", "-q", bare.to_str().unwrap(), "alpha"]);
    g(&root, &["clone", "-q", bare.to_str().unwrap(), "beta"]);
    commit(&b, "b.txt", "from beta\n", "beta work");
    g(&b, &["push", "-q", "origin", "main"]);
    commit(&a, "a.txt", "from alpha\n", "alpha work");
    // Alpha's working tree: staged, modified, untracked, one stash.
    w(&a.join("stashme.txt"), "x\n");
    g(&a, &["add", "stashme.txt"]);
    g(&a, &["stash", "-q"]);
    w(&a.join("new.txt"), "staged\n");
    g(&a, &["add", "new.txt"]);
    w(&a.join("README.md"), "# seed, edited\n");
    w(&a.join("loose.txt"), "untracked\n");

    let gh = root.join("ghproj");
    std::fs::create_dir_all(&gh).unwrap();
    g(&gh, &["init", "-q"]);
    commit(&gh, "main.rs", "fn main() {}\n", "init");
    g(
        &gh,
        &[
            "remote",
            "add",
            "origin",
            "https://alice:ghp_SECRET123@github.com/acme/ghproj.git",
        ],
    );
    // As if fetched once: the remote has main and HEAD points at it.
    g(&gh, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    g(
        &gh,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    g(&gh, &["branch", "-q", "--set-upstream-to=origin/main"]);
    commit(&gh, "lib.rs", "\n", "unpushed one");
    g(&gh, &["checkout", "-q", "-b", "feature/new"]);
    commit(&gh, "f.rs", "\n", "feature work");

    let lonely = root.join("lonely");
    std::fs::create_dir_all(&lonely).unwrap();
    g(&lonely, &["init", "-q"]);
    commit(&lonely, "notes.md", "# notes\n", "only here");

    let plain = root.join("plain");
    w(&plain.join("package.json"), r#"{"name": "plain"}"#);

    let shelf = root.join("shelf");
    let inner = shelf.join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    g(&inner, &["init", "-q"]);
    commit(&inner, "x.txt", "x\n", "inner");
    g(
        &inner,
        &["remote", "add", "origin", "git@gitlab.com:team/inner.git"],
    );
    Fx {
        root,
        bare,
        a,
        b,
        gh,
        lonely,
        plain,
        shelf,
    }
}

/// Every file under `dir` with its (mtime, size).
fn snapshot(dir: &Path) -> Vec<(PathBuf, u128, u64)> {
    let mut out = vec![];
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        if let Some((m, l)) = stamp(&d) {
            out.push((d.clone(), m, l));
        }
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                todo.push(p);
            } else if let Some((m, l)) = stamp(&p) {
                out.push((p, m, l));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn urls_lose_credentials_and_hosts_are_named() {
    assert_eq!(
        clean_url("https://alice:ghp_x@github.com/acme/a.git"),
        "https://github.com/acme/a.git"
    );
    assert_eq!(
        clean_url("https://ghp_tokenonly@github.com/acme/a.git"),
        "https://github.com/acme/a.git"
    );
    assert_eq!(
        clean_url("https://oauth2:glpat-x@gitlab.com/t/r.git?private_token=y"),
        "https://gitlab.com/t/r.git"
    );
    assert_eq!(
        clean_url("ssh://git:pw@host.example/r.git"),
        "ssh://host.example/r.git"
    );
    assert_eq!(
        clean_url("ssh://git@host.example/r.git"),
        "ssh://git@host.example/r.git"
    );
    assert_eq!(
        clean_url("git@github.com:acme/a.git"),
        "git@github.com:acme/a.git"
    );
    assert_eq!(clean_url("/srv/git/r.git"), "/srv/git/r.git");
    let k = |u: &str| host_kind(u);
    assert_eq!(
        k("git@github.com:acme/a.git"),
        ("github", Some("acme/a".into()))
    );
    assert_eq!(
        k("git@github.com-work:acme/a.git"),
        ("github", Some("acme/a".into()))
    );
    assert_eq!(
        k("https://gitlab.com/t/sub/r.git"),
        ("gitlab", Some("t/sub/r".into()))
    );
    assert_eq!(
        k("https://x@bitbucket.org/t/r.git"),
        ("bitbucket", Some("t/r".into()))
    );
    assert_eq!(k("https://git.example.com/r.git").0, "other");
    assert_eq!(k("ssh://git@git.example.com:2222/r.git").0, "other");
    for l in [
        "/srv/git/r.git",
        "../r.git",
        "file:///srv/r.git",
        "~/r.git",
        "r.git",
    ] {
        assert_eq!(k(l).0, "local", "{l}");
    }
    assert_eq!(ago(30), "just now");
    assert_eq!(ago(3 * 86400), "3 days ago");
}

#[test]
fn facts_of_temp_repos() {
    let fx = fixture("facts");
    // The bare remote as a local path; one commit ahead, behind unknown
    // until a fetch (beta's push is not in alpha's refs).
    let a = facts(&fx.a);
    assert_eq!(a.error, None);
    assert_eq!(a.top, fx.a);
    assert_eq!(a.branch.as_deref(), Some("main"));
    assert_eq!(a.upstream.as_deref(), Some("origin/main"));
    assert_eq!((a.ahead, a.behind), (Some(1), Some(0)));
    assert_eq!(a.unpushed, Some(1));
    assert_eq!((a.staged, a.modified, a.untracked, a.stash), (1, 1, 1, 1));
    assert_eq!(a.remotes.len(), 1);
    assert_eq!(a.remotes[0].host, "local");
    assert!(a.local_only() && !a.on_github() && a.dirty() && a.has_unpushed());
    assert_eq!(a.branch_on_remote, Some(true));
    assert_eq!(a.default_branch.as_deref(), Some("main"));
    assert_eq!(a.default_on_remote, Some(true));
    let last = a.last.clone().unwrap();
    assert_eq!(last.subject, "alpha work");
    assert_eq!(last.author, "Test Person");
    assert!(last.hash.len() >= 7 && last.date.starts_with("20"));
    // A clone was fetched at clone time? No FETCH_HEAD until a fetch.
    assert_eq!(
        a.state_word(),
        "1 unpushed, 1 staged, 1 modified, 1 untracked, 1 stash"
    );

    // Beta pushed: clean and even.
    let b = facts(&fx.b);
    assert_eq!((b.ahead, b.behind, b.unpushed), (Some(0), Some(0), Some(0)));
    assert!(!b.dirty() && !b.has_unpushed());
    assert_eq!(b.state_word(), "clean");

    // A fetch (the explicit refresh) makes alpha one behind as well.
    let (ok, failed) = fetch_all(std::slice::from_ref(&fx.a), Duration::from_secs(20));
    assert_eq!((ok, failed.len()), (1, 0), "{failed:?}");
    let a = facts(&fx.a);
    assert_eq!((a.ahead, a.behind), (Some(1), Some(1)));
    assert!(a.is_behind());
    assert!(a.last_fetch.is_some_and(|t| now_unix() - t < 120));
    assert!(
        a.fetch_word().starts_with("fetched just now"),
        "{}",
        a.fetch_word()
    );

    // GitHub looking remote, never contacted: the token is gone, the
    // feature branch is not on the remote, two commits are unpushed.
    let gh = facts(&fx.gh);
    assert_eq!(gh.remotes[0].url, "https://github.com/acme/ghproj.git");
    assert_eq!(gh.remotes[0].host, "github");
    assert_eq!(gh.remotes[0].repo.as_deref(), Some("acme/ghproj"));
    assert!(!serde_json::to_string(&gh).unwrap().contains("SECRET"));
    assert!(!gh.json().to_string().contains("SECRET"));
    assert!(gh.on_github() && !gh.local_only());
    assert_eq!(gh.branch.as_deref(), Some("feature/new"));
    assert_eq!(gh.upstream, None);
    assert_eq!(gh.branch_on_remote, Some(false));
    assert_eq!(gh.default_branch.as_deref(), Some("main"));
    assert_eq!(gh.unpushed, Some(2));
    assert!(gh.has_unpushed());
    assert_eq!(gh.last_fetch, None);
    assert!(gh.remote_refs_at.is_some());
    assert!(
        gh.fetch_word()
            .starts_with("remote refs from just now, never fetched"),
        "{}",
        gh.fetch_word()
    );
    assert!(gh.line().contains("github acme/ghproj"), "{}", gh.line());
    assert!(gh.line().contains("2 unpushed, branch not on remote"));

    // No remote.
    let l = facts(&fx.lonely);
    assert!(l.remotes.is_empty() && l.local_only() && !l.has_unpushed());
    assert_eq!(l.unpushed, None);
    assert_eq!(l.remote_word(), "no remote");

    // Plain folders are in no repo; a subfolder resolves to its repo.
    assert!(locate(&fx.plain).is_none());
    assert!(locate(&fx.shelf).is_none());
    std::fs::create_dir_all(fx.gh.join("src/deep")).unwrap();
    assert_eq!(repo_top(&fx.gh.join("src/deep")), Some(fx.gh.clone()));
    let inner = facts(&fx.shelf.join("inner"));
    assert_eq!(inner.remotes[0].host, "gitlab");
    assert_eq!(inner.remotes[0].repo.as_deref(), Some("team/inner"));
    assert_eq!(inner.branch_on_remote, Some(false));
    assert_eq!(inner.fetch_word(), "never fetched");
    let _ = (&fx.bare, &fx.root);
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn a_worktree_and_an_unborn_repo() {
    let root = base("wt");
    let r = root.join("main");
    std::fs::create_dir_all(&r).unwrap();
    g(&r, &["init", "-q"]);
    commit(&r, "a", "a\n", "one");
    g(&r, &["worktree", "add", "-q", "-b", "side", "../side"]);
    let side = root.join("side");
    let (t, gd, c) = locate(&side).unwrap();
    assert_eq!(t, side);
    assert_ne!(gd, c);
    assert_eq!(c, crate::fspath::canonical(&r.join(".git")).unwrap());
    let f = compute(&t, &gd, &c, Duration::from_secs(5));
    assert_eq!(f.branch.as_deref(), Some("side"));
    assert_eq!(f.error, None);
    let u = root.join("unborn");
    std::fs::create_dir_all(&u).unwrap();
    g(&u, &["init", "-q"]);
    w(&u.join("x"), "x\n");
    let f = facts(&u);
    assert_eq!(f.last, None);
    assert_eq!(f.untracked, 1);
    assert!(f.state_word().starts_with("no commits yet"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn reading_never_writes_into_a_repo() {
    let fx = fixture("ro");
    // A tracked file touched with the same content: plain `git status`
    // would refresh (rewrite) the index here.
    let readme = fx.b.join("README.md");
    let text = std::fs::read(&readme).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(&readme, &text).unwrap();
    let repos = [&fx.a, &fx.b, &fx.gh, &fx.lonely, &fx.shelf];
    let before: Vec<_> = repos.iter().map(|r| snapshot(r)).collect();
    let bare_before = snapshot(&fx.bare);
    for r in repos.iter().filter(|r| locate(r).is_some()) {
        let _ = facts(r);
    }
    let _ = facts(&fx.shelf.join("inner"));
    // Through the cache and the pool too.
    let sh: crate::project_index::Shared = Default::default();
    let dirs: Vec<PathBuf> = repos.iter().map(|r| r.to_path_buf()).collect();
    update(&sh, Some(&dirs), &Limits::tool(), true);
    for (r, b) in repos.iter().zip(&before) {
        assert!(!r.join(".git/index.lock").exists());
        assert_eq!(&snapshot(r), b, "{} changed", r.display());
    }
    assert_eq!(snapshot(&fx.bare), bare_before);
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn the_cache_recomputes_only_what_changed() {
    let fx = fixture("cache");
    let sh: crate::project_index::Shared = Default::default();
    let dirs = vec![fx.a.clone(), fx.b.clone(), fx.gh.clone(), fx.plain.clone()];
    let st = update(&sh, Some(&dirs), &Limits::tool(), false);
    assert_eq!((st.repos, st.computed, st.reused), (3, 3, 0));
    let st = update(&sh, Some(&dirs), &Limits::tool(), false);
    assert_eq!((st.computed, st.reused), (0, 3));
    // A commit in beta changes its refs: only beta again.
    std::thread::sleep(Duration::from_millis(20));
    commit(&fx.b, "c.txt", "c\n", "more");
    let st = update(&sh, Some(&dirs), &Limits::tool(), false);
    assert_eq!((st.computed, st.reused), (1, 2));
    assert_eq!(sh.lock().unwrap().git[&fx.b].unpushed, Some(1));
    // An edit in the work tree changes no git file: a tab finishing a
    // turn there marks it stale.
    w(&fx.gh.join("edited.rs"), "\n");
    let st = update(&sh, Some(&dirs), &Limits::tool(), false);
    assert_eq!(st.computed, 0);
    assert_eq!(mark_stale(&sh, &fx.gh.join("src")), Some(fx.gh.clone()));
    let st = update(&sh, Some(&dirs), &Limits::tool(), false);
    assert_eq!(st.computed, 1);
    assert_eq!(sh.lock().unwrap().git[&fx.gh].untracked, 1);
    // Too old: recomputed even when nothing changed.
    let old = Limits {
        max_age: Duration::ZERO,
        ..Limits::tool()
    };
    assert_eq!(update(&sh, Some(&dirs), &old, false).computed, 3);
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn a_hung_git_is_cut_off() {
    let fx = fixture("slow");
    // Even a zero budget answers (with an error), never hangs.
    let (t, gd, c) = locate(&fx.b).unwrap();
    let f = compute(&t, &gd, &c, Duration::ZERO);
    assert!(f.error.is_some_and(|e| e.contains("timed out")));
    let _ = std::fs::remove_dir_all(&fx.root);
}

/// The fixture as the assistant sees it: roots from saved descriptions.
fn app_with(fx: &Fx, tag: &str) -> (crate::app::App, PathBuf) {
    let home = std::env::temp_dir().join(format!("godterm-git-app-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    for (p, d) in [
        (&fx.a, "alpha app"),
        (&fx.b, "beta app"),
        (&fx.gh, "the github project"),
        (&fx.lonely, "notes kept only here"),
        (&fx.plain, "a plain folder"),
        (&fx.shelf, "a shelf of repos"),
    ] {
        let v = app.control_call(
            "describe_project",
            &json!({"path": p.to_string_lossy(), "description": d}),
        );
        assert_eq!(v["ok"], true, "{v}");
    }
    app.kick_index(true);
    (app, home)
}

#[test]
fn git_status_answers_which_projects_are_only_local() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture("tool");
    let (mut app, home) = app_with(&fx, "tool");
    {
        let ix = app.project_index.lock().unwrap();
        assert!(ix.roots.contains_key(&fx.shelf), "{:?}", ix.roots.keys());
        assert!(
            ix.roots[&fx.shelf]
                .entries
                .iter()
                .any(|e| e.rel == "inner" && e.git),
            "the repo inside the shelf is noticed"
        );
    }
    let v = app.control_call("git_status", &json!({}));
    assert_eq!(v["ok"], true, "{v}");
    let r = &v["result"];
    let s = &r["summary"];
    println!("{}\n{}", r["table"].as_str().unwrap(), r["say"]);
    assert_eq!(s["git_repos"], 5, "{s}");
    assert_eq!(s["on_github"], 1);
    assert_eq!(s["other_host_only"], 1, "gitlab inner");
    assert_eq!(s["no_remote"], 1);
    assert_eq!(s["local_path_remote_only"], 2, "alpha and beta");
    assert_eq!(s["not_a_git_repo"], 1, "plain");
    assert_eq!(s["folders_of_repos"], 1, "shelf");
    assert_eq!(s["dirty"], 1);
    let table = r["table"].as_str().unwrap();
    assert!(table.lines().count() >= 8, "{table}");
    assert!(!v.to_string().contains("SECRET"));

    let v = app.control_call("git_status", &json!({"filter": "no_remote"}));
    let say = v["result"]["say"].as_str().unwrap().to_string();
    println!("{say}");
    assert!(say.starts_with("4 of 7 projects are only local"), "{say}");
    for n in ["alpha", "beta", "lonely", "plain"] {
        assert!(say.contains(n), "{say}");
    }
    assert!(!say.contains("ghproj") && !say.contains("inner"));

    let v = app.control_call("git_status", &json!({"filter": "not_github"}));
    assert!(v["result"]["say"]
        .as_str()
        .unwrap()
        .starts_with("5 of 7 projects are not on GitHub"));
    let v = app.control_call("git_status", &json!({"filter": "unpushed"}));
    let say = v["result"]["say"].as_str().unwrap();
    assert!(say.contains("alpha") && say.contains("ghproj"), "{say}");
    let v = app.control_call("git_status", &json!({"filter": "uncommitted"}));
    assert_eq!(v["result"]["filter"], "dirty");
    assert!(v["result"]["say"].as_str().unwrap().contains("alpha"));
    let v = app.control_call("git_status", &json!({"filter": "bogus"}));
    assert_eq!(v["ok"], false);

    // One project by name, with its details.
    let v = app.control_call("git_status", &json!({"project": "ghproj"}));
    let repos = v["result"]["repos"].as_array().cloned().unwrap_or_default();
    assert_eq!(repos.len(), 1, "{v}");
    assert_eq!(repos[0]["branch"], "feature/new");
    assert_eq!(repos[0]["on_github"], true);
    assert!(v["result"]["say"]
        .as_str()
        .unwrap()
        .contains("feature/new, github acme/ghproj"));

    // refresh fetches (tests: at once) and alpha is now behind.
    let v = app.control_call("git_status", &json!({"filter": "behind", "refresh": true}));
    assert!(
        v["result"]["fetch"]
            .as_str()
            .unwrap()
            .starts_with("fetched"),
        "{v}"
    );
    let v = app.control_call("git_status", &json!({"filter": "behind"}));
    assert!(
        v["result"]["say"].as_str().unwrap().contains("alpha"),
        "{v}"
    );

    // find and get_state carry the cached git line.
    let v = app.control_call("find", &json!({"query": "ghproj"}));
    let hit = v["result"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["type"] == "project")
        .cloned()
        .unwrap();
    assert!(
        hit["git"].as_str().unwrap().contains("github acme/ghproj"),
        "{hit}"
    );
    let st = app.control_call("get_state", &json!({}));
    let pg = st["result"]["projects_git"].as_str().unwrap();
    assert!(pg.starts_with("7 projects: 1 on GitHub"), "{pg}");
    let _ = std::fs::remove_dir_all(&fx.root);
    let _ = std::fs::remove_dir_all(&home);
}
