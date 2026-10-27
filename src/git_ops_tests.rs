use super::*;

fn base(tag: &str) -> PathBuf {
    let b = std::env::temp_dir().join(format!("godterm-ops-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&b);
    std::fs::create_dir_all(&b).unwrap();
    crate::fspath::canonical(&b).unwrap()
}

pub(crate) fn g(dir: &Path, args: &[&str]) -> String {
    let home = std::env::temp_dir().join(format!("godterm-ops-home-{}", std::process::id()));
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn w(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn fixture_commit(dir: &Path, file: &str, text: &str, msg: &str) {
    w(&dir.join(file), text);
    g(dir, &["add", file]);
    g(dir, &["commit", "-q", "-m", msg]);
}

pub(crate) struct Fx {
    pub root: PathBuf,
    pub bare: PathBuf,
    pub a: PathBuf,
    pub b: PathBuf,
}

pub(crate) fn fixture(tag: &str) -> Fx {
    let root = base(tag);
    let bare = root.join("remote.git");
    std::fs::create_dir_all(&bare).unwrap();
    g(&bare, &["init", "-q", "--bare", "-b", "main"]);
    let url = crate::fspath::file_url(&bare);
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    g(&seed, &["init", "-q", "-b", "main"]);
    fixture_commit(&seed, "README.md", "hi\n", "first");
    g(&seed, &["remote", "add", "origin", &url]);
    g(&seed, &["push", "-q", "-u", "origin", "main"]);
    g(&root, &["clone", "-q", &url, "a"]);
    g(&root, &["clone", "-q", &url, "b"]);
    Fx {
        a: root.join("a"),
        b: root.join("b"),
        bare,
        root,
    }
}

fn remote_head(fx: &Fx, branch: &str) -> String {
    g(&fx.bare, &["rev-parse", &format!("refs/heads/{branch}")])
}

fn only_forward(fx: &Fx, before: &[(String, String)]) {
    for (r, old) in before {
        let now = g(&fx.bare, &["rev-parse", r]);
        let out = Command::new("git")
            .args(["merge-base", "--is-ancestor", old, &now])
            .current_dir(&fx.bare)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(out.success(), "{r} moved backwards: {old} -> {now}");
    }
}

fn refs(fx: &Fx) -> Vec<(String, String)> {
    g(
        &fx.bare,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )
    .lines()
    .filter_map(|l| l.split_once(' '))
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect()
}

#[test]
fn commit_push_pull_sync() {
    let fx = fixture("cps");
    let a = Ctx::new(&fx.a);
    let b = Ctx::new(&fx.b);

    w(&fx.a.join("x.txt"), "x\n");
    assert_eq!(
        commit(&a, "nothing staged", false),
        Err(Fail::Msg("nothing to commit".into()))
    );
    let said = commit(&a, "Add x", true).unwrap();
    assert!(said.starts_with("Committed"), "{said}");
    assert!(!tracked_dirty(&a));
    assert_eq!(counts(&a), Some((1, 0)));
    let before = refs(&fx);
    assert_eq!(push(&a).unwrap(), "Pushed main to origin.");
    only_forward(&fx, &before);
    assert_eq!(remote_head(&fx, "main"), g(&fx.a, &["rev-parse", "HEAD"]));

    assert_eq!(pull(&b).unwrap(), "Pulled (fast forward).");
    assert!(fx.b.join("x.txt").exists());
    assert_eq!(pull(&b).unwrap(), "Already up to date.");

    fixture_commit(&fx.b, "y.txt", "y\n", "Add y");
    push(&b).unwrap();
    assert_eq!(sync(&a).unwrap(), "Synced: pulled 1.");
    fixture_commit(&fx.a, "z.txt", "z\n", "Add z");
    let before = refs(&fx);
    assert_eq!(sync(&a).unwrap(), "Synced: pushed 1.");
    only_forward(&fx, &before);
    assert_eq!(sync(&a).unwrap(), "In sync.");
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn diverged_goes_to_its_own_branch() {
    let fx = fixture("div");
    let a = Ctx::new(&fx.a);
    let b = Ctx::new(&fx.b);
    fixture_commit(&fx.b, "b.txt", "b\n", "B's work");
    push(&b).unwrap();
    fixture_commit(&fx.a, "a.txt", "a\n", "A's work");
    let before = refs(&fx);
    let main_before = remote_head(&fx, "main");
    assert_eq!(
        push(&a),
        Err(Fail::Diverged {
            ahead: 1,
            behind: 1
        })
    );
    assert_eq!(
        pull(&a),
        Err(Fail::Diverged {
            ahead: 1,
            behind: 1
        })
    );
    assert!(matches!(sync(&a), Err(Fail::Diverged { .. })));
    assert!(Fail::Diverged {
        ahead: 1,
        behind: 1
    }
    .to_string()
    .contains("Diverged: 1 local, 1 remote"));
    let said = push_as_branch(&a, "Alice's MacBook.local", "20261025").unwrap();
    assert!(
        said.contains("godterm/alice-s-macbook/main-20261025"),
        "{said}"
    );
    assert_eq!(remote_head(&fx, "main"), main_before, "main untouched");
    assert_eq!(
        remote_head(&fx, "godterm/alice-s-macbook/main-20261025"),
        g(&fx.a, &["rev-parse", "HEAD"])
    );

    fixture_commit(&fx.a, "a2.txt", "a2\n", "more");
    let said = push_as_branch(&a, "Alice's MacBook.local", "20261025").unwrap();

    assert!(
        said.contains("godterm/alice-s-macbook/main-20261025"),
        "{said}"
    );
    only_forward(&fx, &before);
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn diverged_branch_name_taken_gets_a_suffix() {
    let fx = fixture("div2");
    let a = Ctx::new(&fx.a);
    let b = Ctx::new(&fx.b);

    g(&fx.b, &["switch", "-q", "-c", "side"]);
    fixture_commit(&fx.b, "s.txt", "s\n", "side");
    g(
        &fx.b,
        &[
            "push",
            "-q",
            "origin",
            "side:refs/heads/godterm/mac/main-20261025",
        ],
    );
    g(&fx.b, &["switch", "-q", "main"]);
    fixture_commit(&fx.b, "b.txt", "b\n", "B");
    push(&b).unwrap();
    fixture_commit(&fx.a, "a.txt", "a\n", "A");
    let before = refs(&fx);
    let said = push_as_branch(&a, "mac", "20261025").unwrap();
    assert!(said.contains("godterm/mac/main-20261025-2"), "{said}");
    only_forward(&fx, &before);
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn branches_switch_and_create() {
    let fx = fixture("br");
    let a = Ctx::new(&fx.a);
    assert!(create_branch(&a, "--force").is_err());
    assert!(create_branch(&a, "+main").is_err());
    create_branch(&a, "feature/one").unwrap();
    assert_eq!(branch(&a).as_deref(), Some("feature/one"));
    w(&fx.a.join("README.md"), "changed\n");
    let e = switch(&a, "main", false).unwrap_err();
    assert!(e.to_string().contains("uncommitted"), "{e}");
    let said = switch(&a, "main", true).unwrap();
    assert!(said.contains("stash"), "{said}");
    assert_eq!(branch(&a).as_deref(), Some("main"));
    let list = branches(&a);
    assert!(list.iter().any(|b| b.name == "main" && b.current));
    assert!(list.iter().any(|b| b.name == "feature/one" && !b.current));
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn log_changes_and_diff() {
    let fx = fixture("log");
    let a = Ctx::new(&fx.a);
    fixture_commit(&fx.a, "n.txt", "1\n2\n", "local one");
    let rows = log(&a, 20);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].unpushed && rows[0].subject == "local one");
    assert!(!rows[1].unpushed);
    w(&fx.a.join("n.txt"), "1\n2\n3\n");
    w(&fx.a.join("new.txt"), "new\n");
    g(&fx.a, &["add", "new.txt"]);
    w(&fx.a.join("loose.txt"), "u\n");
    let c = changes(&a);
    assert_eq!((c.staged_n, c.unstaged_n), (1, 1));
    assert_eq!(c.untracked, vec!["loose.txt".to_string()]);
    assert_eq!(c.added, 2);
    let d = diff_text(&a, false, None, 10_000);
    assert!(d.contains("+3") && d.contains("+new"), "{d}");
    let d = diff_text(&a, true, None, 10_000);
    assert!(!d.contains("+3") && d.contains("+new"), "{d}");
    assert_eq!(outgoing_range(&a), vec!["origin/main..HEAD".to_string()]);
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn init_makes_a_repo() {
    let d = base("init");
    init(&d).unwrap();
    assert!(d.join(".git/HEAD").exists());
    let ctx = Ctx::new(&d);
    assert!(head(&ctx).is_none());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn forbidden_lists() {
    let bad: &[&[&str]] = &[
        &["push", "--force", "origin", "main"],
        &["push", "-f", "origin", "main"],
        &["push", "-uf", "origin", "main"],
        &["push", "--force-with-lease", "origin", "main"],
        &["push", "--force-if-includes", "origin", "main"],
        &["push", "origin", "+main"],
        &["push", "origin", "+refs/heads/a:refs/heads/a"],
        &["push", "origin", ":main"],
        &["push", "--mirror", "origin"],
        &["push", "--delete", "origin", "x"],
        &["push", "-d", "origin", "x"],
        &["push", "--prune", "origin"],
        &["commit", "--amend", "-m", "x"],
        &["pull"],
        &["pull", "--ff-only", "--rebase"],
        &["reset", "--hard", "HEAD~1"],
        &["rebase", "main"],
        &["clean", "-fd"],
        &["update-ref", "refs/heads/main", "HEAD~1"],
        &["filter-branch"],
        &["branch", "-D", "x"],
        &["branch", "-f", "main", "HEAD~1"],
        &["config", "user.name", "x"],
        &["-c", "alias.p=push --force", "p"],
        &["-c", "push.default=force", "push"],
        &["switch", "--discard-changes", "main"],
        &["switch", "-C", "main"],
        &["stash", "drop"],
        &["stash", "clear"],
        &["rm", "file"],
        &["add", "-f", ".env"],
        &["add", "--force", ".env"],
        &["fetch", "origin", "+refs/heads/*:refs/remotes/origin/*"],
        &["remote", "set-url", "origin", "x"],
        &["symbolic-ref", "HEAD", "refs/heads/x"],
        &["checkout", "main"],
        &["gc"],
        &[],
    ];
    for b in bad {
        let a: Vec<String> = b.iter().map(|s| s.to_string()).collect();
        let full = full_args(&Ctx::new(Path::new("/tmp/x")), &a);
        assert!(forbidden(&full).is_some(), "allowed: {b:?}");
    }
    let good: &[&[&str]] = &[
        &[
            "push",
            "--porcelain",
            "-u",
            "origin",
            "refs/heads/main:refs/heads/main",
        ],
        &["pull", "--ff-only", "--no-edit", "--no-rebase"],
        &["commit", "-q", "-m", "-f is a fine word in a message"],
        &["log", "-n20", "--format=%h"],
        &["branch", "--list", "--format=%(refname)"],
        &["config", "--get-regexp", "x"],
        &["stash", "push", "-q", "-m", "note"],
        &["rm", "--cached", "-q", "--", ".env"],
        &["switch", "-q", "-c", "feature"],
    ];
    for g in good {
        let a: Vec<String> = g.iter().map(|s| s.to_string()).collect();
        assert_eq!(forbidden(&a), None, "{g:?}");
    }

    let r = run(
        &Ctx::new(&std::env::temp_dir()),
        &["push".to_string(), "--force".to_string()],
    );
    assert!(matches!(r, Err(Fail::Msg(m)) if m.starts_with("refused")));
}

#[test]
fn no_force_in_any_built_list() {
    let alpha: Vec<char> = "ab-+:f/.@{} ~^_*FORCEforce0".chars().collect();
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut word = |n: usize| -> String {
        let len = 1 + (n % 12);
        (0..len)
            .map(|_| alpha[(next() as usize) % alpha.len()])
            .collect()
    };
    let words: Vec<String> = (0..3000)
        .map(&mut word)
        .chain(
            [
                "--force",
                "-f",
                "+main",
                ":main",
                "--force-with-lease",
                "main:+x",
                "--mirror",
                "-d",
            ]
            .iter()
            .map(|s| s.to_string()),
        )
        .collect();
    let ctx = Ctx {
        account: Some("alice".into()),
        helper: Some(PathBuf::from("/usr/local/bin/godterm")),
        ..Ctx::new(Path::new("/tmp/x"))
    };
    let check = |a: Result<Vec<String>, String>| {
        if let Ok(a) = a {
            let full = full_args(&ctx, &a);
            assert_eq!(forbidden(&full), None, "{full:?}");

            let mut skip = false;
            for x in &a {
                if x == "--" {
                    break;
                }
                if skip {
                    skip = false;
                    continue;
                }
                if x == "-m" || x == "-c" {
                    skip = true;
                    continue;
                }
                let l = x.to_ascii_lowercase();
                assert!(
                    !l.starts_with("--force") && !l.starts_with('+') && x != "-f",
                    "{a:?}"
                );
            }
        }
    };
    let mut built = 0;
    for (i, w) in words.iter().enumerate() {
        let o = &words[(i * 7 + 3) % words.len()];
        for r in [
            push_args(w, o, i % 2 == 0),
            push_args("origin", w, true),
            push_as_args("origin", w, o),
            push_as_args("origin", "main", &diverged_branch(o, w, "20261025")),
            fetch_args(w),
            commit_args(w),
            switch_args(w),
            create_branch_args(w),
            untrack_args(std::slice::from_ref(w)),
            Ok(stash_args(w)),
        ] {
            built += usize::from(r.is_ok());
            check(r);
        }
    }
    for a in [pull_args(), add_all_args(), init_args()] {
        check(Ok(a));
    }
    assert!(built > 1000, "{built}");
}

#[test]
fn credential_options_carry_no_token() {
    let ctx = Ctx {
        account: Some("alice_acme".into()),
        helper: Some(PathBuf::from("/Applications/God Term/godterm")),
        ..Ctx::new(Path::new("/tmp/x"))
    };
    let a = full_args(&ctx, &["push".to_string()]);
    let joined = a.join(" ");
    assert!(joined.contains("credential.helper= "), "{joined}");
    assert!(joined.contains(
        "credential.https://github.com.helper=!'/Applications/God Term/godterm' git-credential --account alice_acme"
    ));
    assert!(joined.contains("credential.useHttpPath=true"));

    let bad = Ctx {
        account: Some("x; rm -rf ~".into()),
        ..ctx.clone()
    };
    assert!(!full_args(&bad, &["push".to_string()])
        .join(" ")
        .contains("credential"));
}

#[test]
fn machine_names() {
    assert_eq!(
        machine_slug("Alices-MacBook-Pro.local"),
        "alices-macbook-pro"
    );
    assert_eq!(machine_slug(""), "machine");
    assert_eq!(machine_slug("..."), "machine");
    assert!(valid_branch(&diverged_branch(
        &machine_name(),
        "main",
        "20261025"
    )));
}
