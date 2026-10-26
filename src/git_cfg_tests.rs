//! The GitHub accounts and repos editor: config.toml written with its
//! comments and other keys kept, the starter set, rule order deciding
//! ties, bad globs and the vault refused, gh account states.

use super::*;

fn h(p: &str) -> PathBuf {
    crate::config::home_dir().join(p)
}

const CONFIG: &str = r##"# My GodTerm config
refresh_secs = 120   # keep this

[[account]]
name = "work"   # the work login
label = "Work"
color = "#7a8b99"
cwd = "~/code"

[git]
undefined = "ask"
fetch_every_min = 15   # mine

[git.owners]
"alice" = "alice"   # personal
"old-org" = "someone"

# The labs rule (keep this comment)
[[git.rules]]
match = "~/labs/**"
owner = "alice-labs"   # the org

[[git.rules]]
match = "~/**"
owner = "alice"

[web]
enabled = false   # after the rules
"##;

#[test]
fn toml_round_trip_keeps_comments_and_other_keys() {
    let cfg = crate::config::Config::parse(CONFIG).unwrap();
    let mut d = Draft::from_cfg(&cfg.git);
    // Nothing changed: the text is the same, byte for byte.
    assert_eq!(write_toml(CONFIG, &d).unwrap(), CONFIG);
    // Edit: drop an owner, add one, change an account, reorder the rules
    // and add one.
    d.owners.retain(|r| r.owner != "old-org");
    d.owners.push(OwnerRow {
        owner: "alice-labs".into(),
        account: "alice".into(),
    });
    d.rules.reverse();
    d.rules.push(GitRule {
        pattern: "~/acme/**".into(),
        owner: "choose".into(),
        choices: vec!["acme-org".into(), "alice".into()],
        state: Some("exclude".into()),
    });
    let out = write_toml(CONFIG, &d).unwrap();
    for keep in [
        "# My GodTerm config",
        "refresh_secs = 120   # keep this",
        "name = \"work\"   # the work login",
        "fetch_every_min = 15   # mine",
        "\"alice\" = \"alice\"   # personal",
        "# The labs rule (keep this comment)",
        "owner = \"alice-labs\"   # the org",
        "enabled = false   # after the rules",
    ] {
        assert!(out.contains(keep), "lost {keep:?}:\n{out}");
    }
    assert!(!out.contains("old-org"), "{out}");
    let back = crate::config::Config::parse(&out).unwrap();
    assert_eq!(back.refresh_secs, 120);
    assert_eq!(back.git.fetch_every_min, 15);
    assert_eq!(back.accounts.len(), 1);
    let pats: Vec<&str> = back.git.rules.iter().map(|r| r.pattern.as_str()).collect();
    assert_eq!(pats, ["~/**", "~/labs/**", "~/acme/**"], "{out}");
    assert_eq!(back.git.rules[2].state.as_deref(), Some("exclude"));
    assert_eq!(back.git.owners["alice-labs"], "alice");
    // The rules stay where they were in the file, before [web].
    let web = out.find("[web]").unwrap();
    assert!(out.rfind("[[git.rules]]").unwrap() < web, "{out}");
    // Writing the same again changes nothing.
    assert_eq!(write_toml(&out, &d).unwrap(), out);
}

#[test]
fn placeholders_apply_to_an_empty_config() {
    let text = "# empty\nrefresh_secs = 90\n";
    let cfg = crate::config::Config::parse(text).unwrap();
    assert!(Draft::from_cfg(&cfg.git).is_empty());
    let p = sample("your-org");
    assert_eq!(p.owners.len(), 4);
    assert!(p.owners.iter().any(|o| is_placeholder(&o.owner)));
    let out = write_toml(text, &p).unwrap();
    assert!(out.starts_with("# empty\nrefresh_secs = 90\n"), "{out}");
    assert!(out.contains(PLACEHOLDER_NOTE), "{out}");
    let back = crate::config::Config::parse(&out).unwrap();
    let d = Draft::from_cfg(&back.git);
    assert!(!d.is_empty());
    assert_eq!(back.git.owners["your-org"], "alice_acme");
    assert_eq!(back.git.owners["bob"], "bob");
    assert_eq!(back.git.rules.len(), 3);
    assert_eq!(back.git.rules[1].owner, "choose");
    assert_eq!(back.git.rules[1].state.as_deref(), Some("exclude"));
    // It resolves as the user described.
    let r = preview_one(&back.git, &h("labs/team-hub"));
    assert_eq!(r.line, "~/labs/team-hub -> alice-labs (alice)");
    let r = preview_one(&back.git, &h("acme/grid"));
    assert_eq!(r.choices, ["your-org", "alice"]);
    assert!(r.state_exclude);
    assert_eq!(
        preview_one(&back.git, &h("notes")).owner.as_deref(),
        Some("alice")
    );
    // The placeholder owner warns until it is renamed.
    let c = check(&d, None);
    assert!(c.ok(), "{c:?}");
    assert!(c
        .warnings
        .iter()
        .any(|w| w.contains("your-org is a placeholder")));
    let mut real = d.clone();
    real.owners
        .iter_mut()
        .find(|o| o.owner == "your-org")
        .unwrap()
        .owner = "acme-corp".into();
    for r in &mut real.rules {
        for c in &mut r.choices {
            if c == "your-org" {
                *c = "acme-corp".into();
            }
        }
    }
    assert!(!check(&real, None)
        .warnings
        .iter()
        .any(|w| w.contains("placeholder")));
    let out2 = write_toml(&out, &real).unwrap();
    assert!(!out2.contains(PLACEHOLDER_NOTE), "{out2}");
}

#[test]
fn reordering_rules_changes_which_one_decides() {
    // Two rules as specific as each other: the higher one wins the tie.
    let mut d = Draft {
        owners: vec![
            OwnerRow {
                owner: "alice-labs".into(),
                account: "alice".into(),
            },
            OwnerRow {
                owner: "bob".into(),
                account: "bob".into(),
            },
        ],
        rules: vec![
            GitRule {
                pattern: "~/labs/**".into(),
                owner: "alice-labs".into(),
                ..Default::default()
            },
            GitRule {
                pattern: "~/labs/*".into(),
                owner: "bob".into(),
                ..Default::default()
            },
        ],
        binds: vec![],
    };
    let base = GitCfg::default();
    let dir = h("labs/team-hub");
    let p = preview(&d, &base, std::slice::from_ref(&dir), &Store::default());
    assert_eq!(p[0].owner.as_deref(), Some("alice-labs"));
    assert_eq!(p[0].rule, Some(0));
    d.rules.swap(0, 1);
    let p = preview(&d, &base, std::slice::from_ref(&dir), &Store::default());
    assert_eq!(p[0].owner.as_deref(), Some("bob"));
    assert_eq!(p[0].line, "~/labs/team-hub -> bob (bob)");
    assert_eq!(p[0].rule, Some(0));
    // The confirmation says the order changed.
    let old = Draft {
        rules: vec![d.rules[1].clone(), d.rules[0].clone()],
        ..d.clone()
    };
    let lines = diff(&old, &d, &Store::default());
    assert!(
        lines.iter().any(|l| l.starts_with("Rules in order")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("1. ~/labs/* -> bob")),
        "{lines:?}"
    );
    // A more specific rule still beats a higher, broader one.
    d.rules.insert(
        0,
        GitRule {
            pattern: "~/**".into(),
            owner: "alice".into(),
            ..Default::default()
        },
    );
    let p = preview(&d, &base, std::slice::from_ref(&dir), &Store::default());
    assert_eq!(p[0].owner.as_deref(), Some("bob"));
    // No rule: undefined.
    let p = preview(&d, &base, &[PathBuf::from("/opt/x")], &Store::default());
    assert_eq!(p[0].rule, None);
    assert!(p[0].line.ends_with("-> undefined (no rule covers it)"));
}

fn rule(p: &str) -> Draft {
    Draft {
        owners: vec![OwnerRow {
            owner: "alice".into(),
            account: "alice".into(),
        }],
        rules: vec![GitRule {
            pattern: p.into(),
            owner: "alice".into(),
            ..Default::default()
        }],
        binds: vec![],
    }
}

#[test]
fn invalid_globs_are_refused() {
    for bad in ["~/[abc/**", "work/**", "", "  ", "**/x"] {
        let c = check(&rule(bad), None);
        assert!(!c.ok(), "{bad:?} was accepted");
        let tmp = std::env::temp_dir().join(format!("gitcfg-bad-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("config.toml");
        std::fs::write(&path, "refresh_secs = 90\n").unwrap();
        assert!(save(&path, &rule(bad), None).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "refresh_secs = 90\n"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
    for good in ["~/**", "~/labs/**", "/opt/work/*", "~/a?c/**", "~/[ab]x/**"] {
        let c = check(&rule(good), None);
        assert!(c.ok(), "{good:?}: {c:?}");
    }
    // Bad names and choose without choices.
    let mut d = rule("~/**");
    d.owners[0].owner = "-bad".into();
    d.rules[0].owner = "choose".into();
    d.rules[0].state = Some("maybe".into());
    let c = check(&d, None);
    assert_eq!(c.errors.len(), 3, "{c:?}");
}

#[test]
fn the_vault_is_refused_as_a_glob_or_a_folder() {
    for v in [
        "/Volumes/Private/**",
        "/Volumes/Private",
        "/Volumes/Private/work/*",
        "/Volumes/**",
        "/Volumes/P*/**",
        "/**",
    ] {
        let c = check(&rule(v), None);
        assert!(c.errors.iter().any(|e| e.contains("vault")), "{v:?}: {c:?}");
    }
    assert!(check(&rule("/Volumes/Other/**"), None).ok());
    // A binding there too.
    let d = Draft {
        binds: vec![BindEdit {
            key: "/Volumes/Private/x".into(),
            binding: Some(Binding {
                owner: "alice".into(),
                repo: "x".into(),
                account: "alice".into(),
                ..Default::default()
            }),
        }],
        ..Default::default()
    };
    assert!(!check(&d, None).ok());
    let mut s = Store::default();
    assert!(apply_binds(&mut s, &d).is_err());
    assert!(s.projects.is_empty());
}

#[test]
fn gh_accounts_and_their_logins() {
    let text = "github.com\n  ✓ Logged in to github.com account alice (keyring)\n  - Active account: true\n  - Token: gho_************************************\n  X Failed to log in to github.com account alice_acme (keyring)\n  - Active account: false\n  - The token in keyring is invalid.\n  ✓ Logged in to github.com account bob (keyring)\n  - Active account: false\n";
    let a = parse_accounts(text);
    assert_eq!(a.len(), 3);
    assert_eq!(
        a[0],
        Acct {
            name: "alice".into(),
            logged_in: true,
            active: true
        }
    );
    assert!(!a[1].logged_in);
    assert!(a[2].logged_in && !a[2].active);
    assert!(!serde_json::to_string(&a).unwrap().contains("gho_"));
    // An owner whose account is not logged in warns; the save still goes.
    let d = sample("your-org");
    let c = check(&d, Some(&a));
    assert!(c.ok());
    assert!(c
        .warnings
        .iter()
        .any(|w| w.starts_with("alice_acme (for your-org) is not logged in")));
    assert!(!c.warnings.iter().any(|w| w.starts_with("alice (for")));
}

#[test]
fn the_diff_names_each_change() {
    let old = Draft::default();
    let new = sample("your-org");
    let mut store = Store::default();
    let lines = diff(&old, &new, &store);
    assert!(
        lines.contains(&"+ owner your-org -> alice_acme (placeholder)".to_string()),
        "{lines:?}"
    );
    assert!(lines.iter().any(|l| l == "+ rule ~/labs/** -> alice-labs"));
    assert!(lines.iter().any(|l| l.starts_with(
        "+ rule ~/acme/** -> choose your-org or alice, conversations and .env kept out"
    )));
    assert!(diff(&new, &new, &store).is_empty());
    // Binding edits.
    store.projects.insert(
        "~/a".into(),
        Binding {
            owner: "alice".into(),
            repo: "a".into(),
            account: "alice".into(),
            ..Default::default()
        },
    );
    let mut d = new.clone();
    d.binds = vec![
        BindEdit {
            key: "~/a".into(),
            binding: None,
        },
        BindEdit {
            key: "~/b".into(),
            binding: Some(Binding {
                owner: "alice-labs".into(),
                repo: "b".into(),
                account: "alice".into(),
                ..Default::default()
            }),
        },
    ];
    let lines = diff(&new, &d, &store);
    assert_eq!(
        lines,
        [
            "- unbind ~/a (was alice/a as alice)",
            "+ bind ~/b -> alice-labs/b as alice"
        ]
    );
    let after = with_binds(&store, &d);
    assert!(after.projects.contains_key("~/b") && !after.projects.contains_key("~/a"));
}

#[test]
fn the_shipped_starter_set_is_generic_examples() {
    let p = placeholders();
    assert!(p.owners.iter().all(|o| is_placeholder(&o.owner)), "{p:?}");
    assert_eq!(p.rules[0].pattern, "~/work/**");
    assert_eq!(p.rules[0].owner, "your-org");
    assert_eq!(p.rules[1].pattern, "~/**");
    assert_eq!(p.rules[1].owner, "your-github-user");
    assert!(check(&p, None).ok());
}

#[test]
fn the_starter_set_is_built_from_the_gh_accounts() {
    let a = |name: &str, active: bool| Acct {
        name: name.into(),
        logged_in: true,
        active,
    };
    assert_eq!(from_accounts(&[]), None);
    let d = from_accounts(&[a("alice", false), a("bob", true)]).unwrap();
    let owners: Vec<(&str, &str)> = d
        .owners
        .iter()
        .map(|o| (o.owner.as_str(), o.account.as_str()))
        .collect();
    assert_eq!(
        owners,
        [("alice", "alice"), ("bob", "bob"), ("your-org", "bob")]
    );
    assert_eq!(d.rules[0].owner, "your-org");
    assert_eq!(d.rules.last().unwrap().pattern, "~/**");
    assert_eq!(d.rules.last().unwrap().owner, "bob", "the active account");
    // No account active: the first one.
    let d = from_accounts(&[a("alice", false), a("bob", false)]).unwrap();
    assert_eq!(d.rules.last().unwrap().owner, "alice");
}

#[test]
fn a_suggestions_file_reads_like_the_draft() {
    let s = parse_suggestions(
        "placeholders = [\"acme-org\"]\n\n[[owners]]\nowner = \"acme-org\"\naccount = \"alice_acme\"\n\n[[rules]]\nmatch = \"~/acme/**\"\nowner = \"choose\"\nchoices = [\"acme-org\", \"alice\"]\nstate = \"exclude\"\n",
    )
    .unwrap();
    assert_eq!(s.placeholders, ["acme-org"]);
    assert_eq!(s.owners[0].account, "alice_acme");
    assert_eq!(s.rules[0].pattern, "~/acme/**");
    assert_eq!(s.rules[0].choices, ["acme-org", "alice"]);
    assert_eq!(s.rules[0].state.as_deref(), Some("exclude"));
    assert_eq!(parse_suggestions("owners = 3"), None);
}
