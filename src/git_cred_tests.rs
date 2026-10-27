use super::*;

struct Stub {
    dir: PathBuf,
    gh: PathBuf,
}

impl Stub {
    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("log")).unwrap_or_default()
    }
}

fn stub(tag: &str, vis: &str) -> Stub {
    let dir = std::env::temp_dir().join(format!("godterm-gh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let d = crate::test_sh::sh_path(&dir);
    let script = format!(
        r#"#!/bin/sh
echo "ARGS $*" >> "{d}/log"
if [ -n "$GH_TOKEN" ]; then echo "ENV GH_TOKEN=$GH_TOKEN" >> "{d}/log"; fi
case "$1 $2" in
  "auth token")
    case "$*" in
      *"--user alice_acme"*) echo "gho_STUBacme000000000000000000000000000"; exit 0;;
      *"--user alice"*) echo "gho_STUBalice00000000000000000000000000"; exit 0;;
      *) echo "no oauth token found for github.com account" >&2; exit 1;;
    esac;;
  "auth status")
    echo "github.com"
    echo "  ✓ Logged in to github.com account alice (keyring)"
    echo "  - Active account: true"
    echo "  ✓ Logged in to github.com account alice_acme (keyring)"
    exit 0;;
  "repo view")
    if [ -f "{d}/created" ]; then echo '{{"visibility":"{vis}","nameWithOwner":"x/y"}}'; exit 0; fi
    echo "GraphQL: Could not resolve to a Repository with the name 'x/y'." >&2; exit 1;;
  "repo create")
    touch "{d}/created"; echo "https://github.com/x/y"; exit 0;;
esac
echo "unexpected" >&2
exit 2
"#
    );
    let gh = crate::test_sh::program(&dir, "gh", &script);
    Stub { dir, gh }
}

#[test]
fn helper_protocol() {
    let s = stub("proto", "PRIVATE");
    let req = "protocol=https\nhost=github.com\npath=alice/x.git\n\n";
    let out = respond("get", req, "alice", &s.gh).unwrap();
    assert_eq!(
        out,
        "username=alice\npassword=gho_STUBalice00000000000000000000000000\n"
    );

    let log = s.log();
    assert!(log.contains("ARGS auth token --hostname github.com --user alice"));
    assert!(!log.contains("switch"));

    let before = s.log();
    for (op, r) in [
        ("get", "protocol=https\nhost=gitlab.com\n"),
        ("get", "protocol=http\nhost=github.com\n"),
        ("store", req),
        ("erase", req),
    ] {
        assert_eq!(respond(op, r, "alice", &s.gh).unwrap(), "");
    }
    assert_eq!(s.log(), before);

    let e = respond("get", req, "nobody", &s.gh).unwrap_err();
    assert!(e.contains("no token for nobody"), "{e}");
    assert!(respond("get", req, "bad name;", &s.gh).is_err());
    let _ = std::fs::remove_dir_all(&s.dir);
}

#[test]
fn gh_token_is_env_never_argv() {
    let s = stub("env", "PRIVATE");
    let c = gh_command(
        &s.gh,
        "alice_acme",
        &["repo", "view", "x/y", "--json", "visibility"],
    )
    .unwrap();
    let argv: Vec<String> = c
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(argv.iter().all(|a| !a.contains("gho_")), "{argv:?}");
    let env: Vec<(String, Option<String>)> = c
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    assert!(env
        .iter()
        .any(|(k, v)| k == "GH_TOKEN"
            && v.as_deref() == Some("gho_STUBacme000000000000000000000000000")));

    assert!(env.iter().any(|(k, v)| k == "GITHUB_TOKEN" && v.is_none()));
    let _ = std::fs::remove_dir_all(&s.dir);
}

#[test]
fn create_is_always_private_and_checked() {
    let dir = std::env::temp_dir().join("proj dir");
    let a = create_args("alice-labs", "team-hub", &dir, "origin").unwrap();
    assert!(a.contains(&"--private".to_string()));
    for bad in ["--public", "--internal", "--push"] {
        assert!(!a.contains(&bad.to_string()));
    }
    assert!(create_args("labs labs", "x", &dir, "origin").is_err());
    assert!(create_args("o", "--public", &dir, "origin").is_err());

    for bad in [
        &["auth", "switch", "--user", "x"][..],
        &["auth", "login"],
        &["auth", "login", "--hostname", "github.com", "--with-token"],
        &[
            "auth",
            "login",
            "--hostname",
            "example.com",
            "--git-protocol",
            "https",
            "--web",
        ],
        &["auth", "logout"],
        &["repo", "delete", "o/x"],
        &["repo", "edit", "o/x", "--visibility", "public"],
        &["repo", "create", "o/x", "--public"],
        &["repo", "create", "o/x"],
        &["repo", "create", "o/x", "--private", "--push"],
        &["api", "repos/o/x", "-X", "DELETE"],
    ] {
        assert!(gh_args_ok(bad).is_err(), "{bad:?}");
    }
    assert!(gh_args_ok(&crate::gh_login::ARGS).is_ok());
    assert!(gh_args_ok(&[
        "repo",
        "create",
        "o/x",
        "--private",
        "--source",
        ".",
        "--remote",
        "origin"
    ])
    .is_ok());

    let s = stub("create", "PRIVATE");
    let src = s.dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    assert_eq!(visibility(&s.gh, "alice", "alice", "y").unwrap(), None);
    let said = create_private(&s.gh, "alice", "alice", "y", &src, "origin").unwrap();
    assert!(said.contains("private, checked"), "{said}");
    let log = s.log();
    let create = log.lines().find(|l| l.contains("repo create")).unwrap();
    assert!(
        create.contains("--private") && !create.contains("--public"),
        "{create}"
    );
    assert!(
        !create.contains("gho_"),
        "token on a command line: {create}"
    );
    assert!(log.contains("ENV GH_TOKEN=gho_STUBalice"), "{log}");
    let _ = std::fs::remove_dir_all(&s.dir);

    let s = stub("public", "PUBLIC");
    let src = s.dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let e = create_private(&s.gh, "alice", "alice", "y", &src, "origin").unwrap_err();
    assert!(e.contains("not PRIVATE"), "{e}");
    let _ = std::fs::remove_dir_all(&s.dir);
}

#[test]
fn accounts_from_status() {
    let s = stub("status", "PRIVATE");
    assert_eq!(
        accounts(&s.gh),
        vec!["alice".to_string(), "alice_acme".to_string()]
    );
    assert!(parse_status("nothing here").is_empty());
    let _ = std::fs::remove_dir_all(&s.dir);
}
