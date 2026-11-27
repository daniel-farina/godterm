use std::cell::RefCell;
use std::path::PathBuf;

use crate::browser::ChromeProfile;
use crate::open_ext::*;
use crate::tab_browser::{Cmd, Os};

fn prof(dir: &str, name: &str) -> ChromeProfile {
    ChromeProfile {
        dir: dir.into(),
        name: name.into(),
        ..Default::default()
    }
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("godterm-openext-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn mac_with(tag: &str, apps: &[&str]) -> Where {
    let d = scratch(tag);
    for a in apps {
        std::fs::create_dir_all(d.join(a)).unwrap();
    }
    Where {
        os: Os::Mac,
        app_dirs: vec![d.clone()],
        bin_dirs: vec![],
        chrome_root: d.join("nochrome"),
    }
}

#[test]
fn only_http_and_https_are_opened_elsewhere() {
    assert_eq!(web_url("localhost:3000").unwrap(), "http://localhost:3000");
    assert_eq!(
        web_url("https://example.com/a?b=1").unwrap(),
        "https://example.com/a?b=1"
    );
    for bad in [
        "file:///etc/passwd",
        "chrome://settings",
        "javascript:alert(1)",
        "about:blank",
        "data:text/html,x",
        "",
        "a b",
    ] {
        assert!(web_url(bad).is_err(), "{bad}");
    }
}

#[test]
fn chrome_argv_is_an_array_with_the_profile_flag() {
    let known = [prof("Default", "Me"), prof("Profile 2", "Work")];
    let c = plan(
        Os::Mac,
        Kind::Chrome,
        Some("Profile 2"),
        "https://example.com/?a=1&b=2",
        &known,
    )
    .unwrap();
    assert_eq!(c.prog, "open");
    assert_eq!(
        c.args,
        [
            "-na",
            "Google Chrome",
            "--args",
            "--profile-directory=Profile 2",
            "https://example.com/?a=1&b=2"
        ]
    );
    let c = plan(Os::Mac, Kind::Chrome, None, "http://localhost:1", &[]).unwrap();
    assert_eq!(
        c.args,
        ["-na", "Google Chrome", "--args", "http://localhost:1"]
    );
}

#[test]
fn unknown_or_odd_profiles_and_urls_are_refused() {
    let known = [prof("Default", "Me")];
    assert!(plan(
        Os::Mac,
        Kind::Chrome,
        Some("Profile 9"),
        "https://a.com",
        &known
    )
    .is_err());
    assert!(plan(Os::Mac, Kind::Chrome, Some("../x"), "https://a.com", &[]).is_err());
    assert!(plan(Os::Mac, Kind::Chrome, Some("Default"), "file:///x", &known).is_err());
    assert!(plan(Os::Linux, Kind::Safari, None, "https://a.com", &[]).is_err());
}

#[test]
fn safari_argv() {
    let c = plan(Os::Mac, Kind::Safari, None, "https://example.com", &[]).unwrap();
    assert_eq!(
        c,
        Cmd {
            prog: "open".into(),
            args: vec!["-a".into(), "Safari".into(), "https://example.com".into()]
        }
    );
}

#[test]
fn installed_browsers_are_found_by_app_name() {
    let w = mac_with("a", &["Google Chrome.app"]);
    assert!(chrome_installed(&w));
    assert!(!safari_installed(&w));
    let l = listing(&w, &[prof("Default", "Me")]);
    assert_eq!(l["chrome"], true);
    assert_eq!(l["safari"], false);
    assert_eq!(l["profiles"][0]["name"], "Me");
    let w = mac_with("b", &["Safari.app"]);
    assert!(!chrome_installed(&w));
    assert!(safari_installed(&w));
    assert_eq!(
        listing(&w, &[prof("Default", "Me")])["profiles"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let mut w = mac_with("c", &["Safari.app"]);
    w.os = Os::Linux;
    assert!(!safari_installed(&w));
}

#[test]
fn open_runs_the_injected_launcher_once_and_never_for_refusals() {
    let w = mac_with("d", &["Google Chrome.app", "Safari.app"]);
    let known = [prof("Default", "Me")];
    let seen = RefCell::new(Vec::new());
    let run = |c: &Cmd| {
        seen.borrow_mut().push(c.clone());
        true
    };
    let m = open_with(
        &w,
        Kind::Chrome,
        Some("Default"),
        "https://a.com",
        &known,
        &run,
    )
    .unwrap();
    assert!(m.contains("Me"));
    assert_eq!(seen.borrow().len(), 1);
    assert!(open_with(&w, Kind::Chrome, Some("Default"), "file:///x", &known, &run).is_err());
    assert!(open_with(&w, Kind::Safari, None, "chrome://x", &[], &run).is_err());
    assert_eq!(seen.borrow().len(), 1);
    let fail = |_: &Cmd| false;
    assert!(open_with(&w, Kind::Safari, None, "https://a.com", &[], &fail).is_err());
    let w2 = mac_with("e", &[]);
    assert!(open_with(&w2, Kind::Chrome, None, "https://a.com", &[], &run).is_err());
    assert_eq!(seen.borrow().len(), 1);
}

#[test]
fn profile_names_come_from_local_state_names_only() {
    let d = scratch("f");
    std::fs::write(
        d.join("Local State"),
        r#"{"profile":{"info_cache":{"Default":{"name":"Me","user_name":"a@b.c"},"Profile 1":{"name":"Work"}}}}"#,
    )
    .unwrap();
    let p = local_profiles(&d);
    assert_eq!(p.len(), 2);
    assert_eq!(p[0].dir, "Default");
    assert_eq!(p[1].name, "Work");
}
