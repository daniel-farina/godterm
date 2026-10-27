use super::*;
use crate::project_git::{Facts, Remote};

fn cfg() -> GitCfg {
    let toml = r#"
undefined = "ask"
[owners]
"alice" = "alice"
"alice-labs" = "alice"
"acme-org" = "alice_acme"

[[rules]]
match = "~/**"
owner = "alice"

[[rules]]
match = "~/labs/**"
owner = "alice-labs"

[[rules]]
match = "~/acme/**"
owner = "choose"
choices = ["acme-org", "alice"]
state = "exclude"

[[rules]]
match = "~/labs/**"
owner = "someone-else"
"#;
    toml::from_str(toml).unwrap()
}

fn h(p: &str) -> PathBuf {
    crate::config::home_dir().join(p)
}

#[test]
fn most_specific_rule_wins_ties_to_the_first() {
    let c = cfg();
    assert_eq!(
        rule_for(&c, &h("labs/team-hub")).unwrap().owner,
        "alice-labs"
    );
    assert_eq!(rule_for(&c, &h("labs")).unwrap().owner, "alice-labs");
    assert_eq!(rule_for(&c, &h("code/x")).unwrap().owner, "alice");
    assert_eq!(rule_for(&c, &h("acme/app")).unwrap().owner, "choose");
    assert!(rule_for(&c, Path::new("/opt/elsewhere")).is_none());
    assert!(!glob_matches("~/labs/**", &h("labsx/a")));
}

#[test]
fn resolution_paths() {
    let c = cfg();
    let s = Store::default();
    let r = resolve(&h("labs/team-hub"), None, &c, &s);
    assert_eq!(r.source, "rule");
    assert_eq!(r.owner.as_deref(), Some("alice-labs"));
    assert_eq!(r.account.as_deref(), Some("alice"));
    assert_eq!(r.repo.as_deref(), Some("team-hub"));
    assert!(r.complete() && r.ask.is_none());

    let r = resolve(&h("acme/Billing App"), None, &c, &s);
    assert_eq!(r.choices, vec!["acme-org", "alice"]);
    assert!(r.owner.is_none() && r.ask.is_some() && r.state_exclude);
    assert_eq!(r.repo.as_deref(), Some("billing-app"));

    let r = resolve(Path::new("/opt/elsewhere/x"), None, &GitCfg::default(), &s);
    assert!(r.undefined() && r.owner.is_none());
    assert!(r.ask.unwrap().contains("No repo defined"));
}

fn gh_facts(url: &str, repo: &str) -> Facts {
    Facts {
        remotes: vec![Remote {
            name: "origin".into(),
            url: url.into(),
            host: "github".into(),
            repo: Some(repo.into()),
        }],
        ..Default::default()
    }
}

#[test]
fn binding_from_remote_and_unknown_owner_asks() {
    let c = cfg();
    let mut s = Store::default();
    let f = gh_facts("https://github.com/alice-labs/hub.git", "alice-labs/hub");
    let r = resolve(&h("code/hub"), Some(&f), &c, &s);
    assert_eq!(r.source, "remote");
    assert!(r.on_github);
    assert_eq!(r.slug().as_deref(), Some("alice-labs/hub"));
    assert_eq!(r.account.as_deref(), Some("alice"));
    let f = gh_facts("git@github.com:stranger/tool.git", "stranger/tool");
    let r = resolve(&h("code/tool"), Some(&f), &c, &s);
    assert!(r.account.is_none());
    assert!(r.ask.as_deref().unwrap().contains("stranger"));

    let _g = crate::config::testing::LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    s.bind(
        &h("code/tool"),
        Binding {
            owner: "stranger".into(),
            repo: "tool".into(),
            account: "alice".into(),
            source: "manual".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let r = resolve(&h("code/tool"), Some(&f), &c, &s);
    assert_eq!(r.account.as_deref(), Some("alice"));
    assert!(s.projects.contains_key("~/code/tool"));

    s.bind(
        Path::new("/opt/elsewhere/x"),
        Binding {
            owner: "alice".into(),
            repo: "x".into(),
            account: "alice".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let r = resolve(Path::new("/opt/elsewhere/x"), None, &GitCfg::default(), &s);
    assert_eq!(r.source, "saved");

    let bad = Binding {
        owner: "a b".into(),
        repo: "x".into(),
        account: "y".into(),
        ..Default::default()
    };
    assert!(s.bind(&h("code/z"), bad).is_err());
}

#[test]
fn vault_and_private_folders_are_refused() {
    let c = cfg();
    let mut s = Store::default();
    for p in [Path::new("/Volumes/Private/proj"), &h(".ssh")] {
        let r = resolve(p, None, &c, &s);
        assert!(r.refused.is_some(), "{}", p.display());
        assert!(!r.complete());
        let b = Binding {
            owner: "alice".into(),
            repo: "x".into(),
            account: "alice".into(),
            ..Default::default()
        };
        assert!(s.bind(p, b).is_err());
    }
    #[cfg(unix)]
    {
        let d = std::env::temp_dir().join(format!("godterm-map-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::os::unix::fs::symlink("/Volumes/Private/p", d.join("link")).unwrap();
        assert!(resolve(&d.join("link"), None, &c, &s).refused.is_some());
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[test]
fn names() {
    assert_eq!(repo_name_for(Path::new("/x/My Cool App")), "my-cool-app");
    assert_eq!(repo_name_for(Path::new("/x/.hidden")), "hidden");
    assert!(valid_owner("alice_acme") && valid_owner("alice-labs"));
    assert!(!valid_owner("-x") && !valid_owner("a/b") && !valid_owner(""));
    assert!(valid_repo("godterm-state") && !valid_repo("..") && !valid_repo("a b"));
}
