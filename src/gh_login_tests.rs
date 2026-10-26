use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

// A fake gh: a shell script in a temp folder, run with a temp
// GH_CONFIG_DIR. The real gh and its login state are never reached.
fn fake(tag: &str, body: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("godterm-ghlogin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("cfg")).unwrap();
    let gh = dir.join("gh");
    std::fs::write(&gh, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, gh)
}

fn wait_for(l: &Login, f: impl Fn(&Info) -> bool) -> Info {
    let t0 = Instant::now();
    loop {
        let i = l.info();
        if f(&i) || t0.elapsed() > Duration::from_secs(10) {
            return i;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn parses_gh_output() {
    assert_eq!(
        parse_code("! First copy your one-time code: ABCD-1234").as_deref(),
        Some("ABCD-1234")
    );
    assert_eq!(parse_code("one-time code: lower-case"), None);
    assert_eq!(parse_code("no code here"), None);
    assert_eq!(
        parse_url("Open this URL to continue in your web browser: https://github.com/login/device")
            .as_deref(),
        Some("https://github.com/login/device")
    );
    assert_eq!(parse_url("see https://example.com/x"), None);
    assert_eq!(
        parse_account("✓ Logged in as alice").as_deref(),
        Some("alice")
    );
    assert_eq!(
        parse_account("✓ Logged in as acme-org now").as_deref(),
        Some("acme-org")
    );
    assert_eq!(parse_account("Logged in as "), None);
}

#[test]
fn runs_the_exact_args_and_reports_code_then_done() {
    let (dir, gh) = fake(
        "ok",
        r#"[ "$*" = "auth login --hostname github.com --git-protocol https --web" ] || { echo "bad args: $*" >&2; exit 9; }
[ -n "$GH_TOKEN" ] && { echo "token leaked" >&2; exit 8; }
[ "$GH_PROMPT_DISABLED" = 1 ] || { echo "prompts on" >&2; exit 7; }
echo "cfg=$GH_CONFIG_DIR" > "$GH_CONFIG_DIR/ran"
echo "! First copy your one-time code: WXYZ-9876" >&2
echo "Open this URL to continue in your web browser: https://github.com/login/device" >&2
sleep 0.3
echo "✓ Authentication complete." >&2
echo "✓ Logged in as alice" >&2
exit 0"#,
    );
    let cfg = dir.join("cfg");
    let l = Login::start(
        &gh,
        &[("GH_CONFIG_DIR", cfg.to_str().unwrap()), ("GH_TOKEN", "")],
    );
    let i = wait_for(&l, |i| i.state == "code");
    assert_eq!(i.state, "code", "{i:?}");
    assert_eq!(i.code.as_deref(), Some("WXYZ-9876"));
    assert_eq!(i.url.as_deref(), Some("https://github.com/login/device"));
    let i = wait_for(&l, |i| !i.running());
    assert_eq!(i.state, "done", "{i:?}");
    assert_eq!(i.account.as_deref(), Some("alice"));
    assert!(std::fs::read_to_string(cfg.join("ran"))
        .unwrap()
        .contains(cfg.to_str().unwrap()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failure_shows_gh_last_line() {
    let (dir, gh) = fake("fail", "echo 'error: authentication failed' >&2\nexit 1");
    let l = Login::start(&gh, &[("GH_CONFIG_DIR", dir.join("cfg").to_str().unwrap())]);
    let i = wait_for(&l, |i| !i.running());
    assert_eq!(i.state, "failed");
    assert_eq!(i.error.as_deref(), Some("error: authentication failed"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_stops_it_and_a_missing_gh_fails() {
    let (dir, gh) = fake(
        "cancel",
        "echo '! First copy your one-time code: AAAA-BBBB' >&2\nexec sleep 30",
    );
    let l = Login::start(&gh, &[("GH_CONFIG_DIR", dir.join("cfg").to_str().unwrap())]);
    assert_eq!(wait_for(&l, |i| i.state == "code").state, "code");
    let t0 = Instant::now();
    l.cancel();
    let i = wait_for(&l, |i| !i.running());
    assert_eq!(i.state, "cancelled");
    assert!(t0.elapsed() < Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(l.info().state, "cancelled", "the waiter keeps it cancelled");
    let _ = std::fs::remove_dir_all(&dir);

    let l = Login::start(Path::new("/nonexistent/gh"), &[]);
    let i = l.info();
    assert_eq!(i.state, "failed");
    assert!(i.error.unwrap().contains("could not run gh"));
}

#[test]
fn the_web_ops_never_reach_a_real_gh() {
    // Under test gh_bin is /nonexistent/gh: the shared login fails at once.
    let gh = crate::git_cred::gh_bin(None);
    let i = start(&gh);
    assert_eq!(i.state, "failed", "{i:?}");
    assert_eq!(poll().state, "failed");
    assert_eq!(cancel().state, "failed");
}
