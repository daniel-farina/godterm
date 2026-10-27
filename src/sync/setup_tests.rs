use super::tests::{fixture_a, root, w};
use super::*;

#[test]
fn preflight_and_plan_warnings_name_the_repo_account_and_secrets() {
    let r = root("ui-preflight");
    let (a, _home) = fixture_a(&r);

    let pf = plan::preflight(&a, "tester", "godterm-state", "tester", &[]);
    assert!(pf.can_init && !pf.can_clone, "{pf:?}");
    assert_eq!(pf.state_repo, "tester/godterm-state");
    assert_eq!(pf.convos, "tester/godterm-convos-YYYY-MM");
    assert_eq!(pf.exists, Some(false));

    let p = plan::plan(&a);
    let ws = p.warnings();
    assert!(
        ws.iter().any(|w| w.contains(".env files (secrets)")),
        "{ws:?}"
    );
    assert!(
        p.env_paths.iter().any(|e| e.ends_with("/.env")),
        "{:?}",
        p.env_paths
    );
    assert!(
        ws.iter().any(|w| w.contains("/Volumes/Private and")),
        "{ws:?}"
    );

    let rp = r.join("remotes");
    git::Remote::Local { base: rp.clone() }
        .create_private("tester", "godterm-state")
        .unwrap();
    let pf = plan::preflight(&a, "tester", "godterm-state", "tester", &[]);
    assert!(!pf.can_init && pf.can_clone, "{pf:?}");
    assert!(pf.warnings.iter().any(|w| w.contains("join it")));

    w(&rp.join("tester/godterm-state.git/visibility"), "PUBLIC\n");
    let pf = plan::preflight(&a, "tester", "godterm-state", "tester", &[]);
    assert!(!pf.can_init && !pf.can_clone, "{pf:?}");
    assert_eq!(pf.visibility.as_deref(), Some("PUBLIC"));
    assert!(pf
        .warnings
        .iter()
        .any(|w| w.contains("PUBLIC, not private")));

    let mut gh = a.clone();
    gh.cfg.sync.remote = "github".into();
    let pf = plan::preflight(&gh, "tester", "godterm-state", "tester", &["other".into()]);
    assert!(!pf.can_init && !pf.can_clone && !pf.logged_in, "{pf:?}");
    assert!(
        pf.warnings[0].contains("gh auth login"),
        "{:?}",
        pf.warnings
    );

    let pf = plan::preflight(&gh, "", "godterm-state", "", &[]);
    assert!(!pf.can_init && pf.warnings[0].contains("no GitHub owner"));

    let mut sc = SyncCfg::default();
    restore::resolve_owner(&mut sc, &["first".into(), "second".into()]);
    assert_eq!((sc.owner.as_str(), sc.account.as_str()), ("first", "first"));
    let mut sc = SyncCfg {
        owner: "org".into(),
        ..Default::default()
    };
    restore::resolve_owner(&mut sc, &["first".into()]);
    assert_eq!((sc.owner.as_str(), sc.account.as_str()), ("org", "org"));
}

#[test]
fn clone_refuses_a_state_repo_that_is_not_private() {
    let r = root("ui-public");
    let (a, _home) = fixture_a(&r);
    restore::init(&a, a.cfg.sync.clone(), &|_| true).unwrap();
    w(
        &r.join("remotes/tester/godterm-state.git/visibility"),
        "PUBLIC\n",
    );
    let b = super::tests::ctx_for(
        &r.join("b"),
        &super::tests::config_text(&r.join("remotes"), false),
    );
    let e = restore::clone(&b, "tester/godterm-state", &Default::default()).unwrap_err();
    assert!(format!("{e:#}").contains("not private"), "{e:#}");

    assert!(!restore::reload(&b).unwrap().sync().enabled);
}
