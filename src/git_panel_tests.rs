//! The Git section's core against temp repos, a local bare remote and a
//! stub gh: warnings for projects that are not backed up, plans that
//! check before anything changes, the typed override for secrets (by the
//! human only), private repo creation, bindings, and the web frames.

use super::*;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::git_ops::tests::{fixture, g};

fn env() -> Env {
    Env {
        cfg: GitCfg::default(),
        helper: None,
        gh: PathBuf::from("/nonexistent/gh"),
        machine: "testmac".into(),
    }
}

fn home(tag: &str) -> PathBuf {
    let h = std::env::temp_dir().join(format!("godterm-gpanel-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&h);
    std::fs::create_dir_all(&h).unwrap();
    crate::config::testing::set_home(&h);
    h
}

fn facts(dir: &Path) -> Option<Facts> {
    facts_for(dir)
}

#[test]
fn warnings_for_what_is_not_backed_up() {
    let fx = fixture("warn");
    // A file:// remote is only a folder here.
    let w = warnings(facts(&fx.a).as_ref());
    assert!(
        w.contains(&"Not backed up: no GitHub repo".to_string()),
        "{w:?}"
    );
    std::fs::write(fx.a.join("n.txt"), "x").unwrap();
    for _ in 0..12 {
        std::fs::write(
            fx.a.join("c.txt"),
            format!("{:?}", std::time::Instant::now()),
        )
        .unwrap();
        g(&fx.a, &["add", "c.txt"]);
        g(&fx.a, &["commit", "-q", "-m", "c"]);
    }
    let w = warnings(facts(&fx.a).as_ref());
    assert!(w.contains(&"12 unpushed".to_string()), "{w:?}");
    assert!(w.contains(&"uncommitted changes".to_string()), "{w:?}");
    // Not a repo at all.
    assert_eq!(
        warnings(None),
        vec!["Not backed up: not in git".to_string()]
    );
    // No remote at all.
    let lonely = fx.root.join("lonely");
    std::fs::create_dir_all(&lonely).unwrap();
    g(&lonely, &["init", "-q", "-b", "main"]);
    let w = warnings(facts(&lonely).as_ref());
    assert!(
        w.contains(&"Not backed up: no GitHub repo".to_string()),
        "{w:?}"
    );
    assert!(w.contains(&"no commits yet".to_string()), "{w:?}");
    // A GitHub remote and nothing pending: no warning.
    let f = Facts {
        remotes: vec![project_git::Remote {
            name: "origin".into(),
            url: "https://github.com/a/b".into(),
            host: "github".into(),
            repo: Some("a/b".into()),
        }],
        last: Some(Default::default()),
        branch_on_remote: Some(true),
        ahead: Some(0),
        unpushed: Some(0),
        ..Default::default()
    };
    assert!(warnings(Some(&f)).is_empty());
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn view_lists_actions_and_undefined_asks() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("view");
    let fx = fixture("view");
    let v = view(&fx.a, &env(), &["alice".into()]);
    assert!(v.is_repo);
    assert_eq!(v.branch, "main");
    assert!(v.resolved.undefined(), "{:?}", v.resolved);
    let ids: Vec<&str> = v.actions.iter().map(|a| a.id).collect();
    for want in [
        "commit", "push", "pull", "sync", "fetch", "branches", "define", "create",
    ] {
        assert!(ids.contains(&want), "{ids:?}");
    }
    let create = v.actions.iter().find(|a| a.id == "create").unwrap();
    assert!(create.disabled.is_some(), "create needs a definition first");
    let commit = v.actions.iter().find(|a| a.id == "commit").unwrap();
    assert_eq!(commit.disabled.as_deref(), Some("nothing to commit"));
    assert!(v.json()["warnings"].to_string().contains("no GitHub repo"));
    // A plain folder offers git init.
    let plain = fx.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let v = view(&plain, &env(), &[]);
    assert!(!v.is_repo && v.actions[0].id == "init");
    // The vault: refused, nothing offered.
    let v = view(Path::new("/Volumes/Private/x"), &env(), &[]);
    assert!(v.refused.is_some() && v.actions.is_empty());
    let _ = std::fs::remove_dir_all(&fx.root);
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn commit_and_push_only_after_confirm_and_typed_override() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("push");
    let fx = fixture("push");
    let e = env();
    let head0 = g(&fx.a, &["rev-parse", "HEAD"]);
    std::fs::write(fx.a.join("a.txt"), "hello\n").unwrap();
    // Preparing changes nothing.
    let p = prepare(
        &fx.a,
        Action::Commit {
            message: "Add a".into(),
            all: true,
        },
        &e,
    );
    assert!(p.can_run(), "{:?}", p.hard_blocks);
    assert_eq!(g(&fx.a, &["rev-parse", "HEAD"]), head0);
    assert!(execute(&p, None, &e).unwrap().starts_with("Committed"));
    // A secret in the next commit: the push needs the typed phrase.
    let key = format!("ghp_{}", "Q".repeat(36));
    std::fs::write(fx.a.join("cfg.txt"), format!("token = {key}\n")).unwrap();
    let p = prepare(
        &fx.a,
        Action::Commit {
            message: "cfg".into(),
            all: true,
        },
        &e,
    );
    execute(&p, None, &e).unwrap();
    let before = g(&fx.bare, &["rev-parse", "refs/heads/main"]);
    let p = prepare(&fx.a, Action::Push, &e);
    assert!(p.needs_typed && p.can_run(), "{p:?}");
    assert!(
        !p.json().to_string().contains(&key),
        "the plan never shows the value"
    );
    let err = execute(&p, None, &e).unwrap_err();
    assert!(err.contains("push anyway"), "{err}");
    assert!(execute(&p, Some("yes"), &e).is_err());
    assert_eq!(
        g(&fx.bare, &["rev-parse", "refs/heads/main"]),
        before,
        "nothing pushed"
    );
    // A newer commit than the check: refused, check again.
    std::fs::write(fx.a.join("b.txt"), "b\n").unwrap();
    let pc = prepare(
        &fx.a,
        Action::Commit {
            message: "b".into(),
            all: true,
        },
        &e,
    );
    execute(&pc, None, &e).unwrap();
    assert!(execute(&p, Some("push anyway"), &e)
        .unwrap_err()
        .contains("changed"));
    let p = prepare(&fx.a, Action::Push, &e);
    let said = execute(&p, Some("Push Anyway "), &e).unwrap();
    assert!(said.starts_with("Pushed"), "{said}");
    assert_eq!(
        g(&fx.bare, &["rev-parse", "refs/heads/main"]),
        g(&fx.a, &["rev-parse", "HEAD"])
    );
    // A secret file staged: the commit itself is refused.
    std::fs::write(fx.a.join(".env"), "X=1\n").unwrap();
    let p = prepare(
        &fx.a,
        Action::Commit {
            message: "env".into(),
            all: true,
        },
        &e,
    );
    assert!(!p.can_run() && p.hard_blocks[0].contains(".env"), "{p:?}");
    assert!(execute(&p, None, &e).is_err());
    let _ = std::fs::remove_dir_all(&fx.root);
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn diverged_then_push_as_branch() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("div");
    let fx = fixture("pdiv");
    let e = env();
    std::fs::write(fx.b.join("b.txt"), "b\n").unwrap();
    g(&fx.b, &["add", "b.txt"]);
    g(&fx.b, &["commit", "-q", "-m", "b"]);
    g(&fx.b, &["push", "-q", "origin", "main"]);
    std::fs::write(fx.a.join("a.txt"), "a\n").unwrap();
    g(&fx.a, &["add", "a.txt"]);
    g(&fx.a, &["commit", "-q", "-m", "a"]);
    let p = prepare(&fx.a, Action::Push, &e);
    let err = execute(&p, None, &e).unwrap_err();
    assert!(err.starts_with("Diverged: 1 local, 1 remote"), "{err}");
    let p = prepare(&fx.a, Action::PushAsBranch, &e);
    assert!(
        p.lines[0].contains("godterm/testmac/main-"),
        "{:?}",
        p.lines
    );
    let said = execute(&p, None, &e).unwrap();
    assert!(said.contains("godterm/testmac/main-"), "{said}");
    let _ = std::fs::remove_dir_all(&fx.root);
    let _ = std::fs::remove_dir_all(&h);
}

/// A stub gh that knows alice and creates repos (logs argv/env).
fn stub_gh(dir: &Path) -> PathBuf {
    let d = crate::test_sh::sh_path(dir);
    let script = format!(
        r#"#!/bin/sh
echo "ARGS $*" >> "{d}/gh.log"
[ -n "$GH_TOKEN" ] && echo "ENV GH_TOKEN" >> "{d}/gh.log"
case "$1 $2" in
  "auth token") echo "gho_STUB0000000000000000000000000000000"; exit 0;;
  "repo view") if [ -f "{d}/made" ]; then echo '{{"visibility":"PRIVATE"}}'; exit 0; fi
     echo "Could not resolve to a Repository" >&2; exit 1;;
  "repo create") touch "{d}/made"; exit 0;;
esac
exit 2
"#
    );
    crate::test_sh::program(dir, "gh", &script)
}

#[test]
fn define_then_create_private_repo() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("create");
    let fx = fixture("create");
    let mut e = env();
    e.gh = stub_gh(&fx.root);
    // Bind first (the user's Define): saved, nothing in the repo changes.
    let p = prepare(
        &fx.a,
        Action::Bind {
            owner: "alice".into(),
            repo: "my-app".into(),
            account: "alice".into(),
        },
        &e,
    );
    assert!(p.can_run(), "{:?}", p.hard_blocks);
    execute(&p, None, &e).unwrap();
    let st = Store::load();
    assert_eq!(st.get(&fx.a).unwrap().repo, "my-app");
    let v = view(&fx.a, &e, &[]);
    assert_eq!(v.resolved.source, "saved");
    assert!(v
        .actions
        .iter()
        .find(|a| a.id == "create")
        .unwrap()
        .disabled
        .is_none());
    // Create: private, checked, bound; no push in this test.
    let p = prepare(
        &fx.a,
        Action::CreateRepo {
            owner: "alice".into(),
            name: "my-app".into(),
            account: "alice".into(),
            push: false,
        },
        &e,
    );
    assert!(p.can_run(), "{:?}", p.hard_blocks);
    assert!(p.lines[0].contains("--private"), "{:?}", p.lines);
    let said = execute(&p, None, &e).unwrap();
    assert!(said.contains("private, checked"), "{said}");
    let log = std::fs::read_to_string(fx.root.join("gh.log")).unwrap();
    let create = log.lines().find(|l| l.contains("repo create")).unwrap();
    assert!(
        create.contains("--private") && !create.contains("--public") && !create.contains("gho_")
    );
    assert!(log.contains("ENV GH_TOKEN"));
    assert!(!log.contains("auth switch"));
    // Now it exists: a second create is refused (use existing instead).
    let p = prepare(
        &fx.a,
        Action::CreateRepo {
            owner: "alice".into(),
            name: "my-app".into(),
            account: "alice".into(),
            push: false,
        },
        &e,
    );
    assert!(p.exists && !p.can_run());
    // Bad names never get that far.
    let p = prepare(
        &fx.a,
        Action::CreateRepo {
            owner: "a b".into(),
            name: "--public".into(),
            account: "x".into(),
            push: false,
        },
        &e,
    );
    assert!(!p.can_run());
    let _ = std::fs::remove_dir_all(&fx.root);
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn web_frames_view_prepare_run() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("web");
    let fx = fixture("web");
    let e = env();
    let r = web_frame(&json!({"op": "view"}), Some(fx.a.clone()), &e, &[]);
    assert_eq!(r["ok"], true);
    assert_eq!(r["view"]["branch"], "main");
    std::fs::write(fx.a.join("w.txt"), "w\n").unwrap();
    let r = web_frame(
        &json!({"op": "prepare", "action": "commit", "message": "From the web", "all": true}),
        Some(fx.a.clone()),
        &e,
        &[],
    );
    assert_eq!(r["ok"], true, "{r}");
    let id = r["plan_id"].as_str().unwrap().to_string();
    assert_eq!(
        g(&fx.a, &["status", "--porcelain"]),
        "?? w.txt",
        "prepare changed nothing"
    );
    let r = web_frame(&json!({"op": "run", "plan_id": id}), None, &e, &[]);
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["said"].as_str().unwrap().starts_with("Committed"));
    // A plan runs once; unknown ids and actions are refused.
    let r = web_frame(&json!({"op": "run", "plan_id": id}), None, &e, &[]);
    assert_eq!(r["ok"], false);
    let r = web_frame(
        &json!({"op": "prepare", "action": "force_push"}),
        Some(fx.a.clone()),
        &e,
        &[],
    );
    assert_eq!(r["ok"], false);
    let _ = std::fs::remove_dir_all(&fx.root);
    let _ = std::fs::remove_dir_all(&h);
}
