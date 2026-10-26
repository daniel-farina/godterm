//! BrowserSkill: parsing, the passive status look, the skill copy and the
//! settings.json merge. Everything runs in temp folders with fake `bsk`
//! scripts; the real ~/.claude, ~/.godterm, ~/.grok, ~/.bsk and Chrome
//! folders are never touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::browser::{self, Daemon, Env, ExtPref};
use crate::browser_skill::{self as skill, Action, Merge, BSK_RULE};
use crate::config::testing::LOCK as ENV_LOCK;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("godterm-browser-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A fake `bsk` that runs `body` (shell) with the arguments in $1 $2 ...
#[cfg(unix)]
fn fake_bsk(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir).unwrap();
    let p = dir.join("bsk");
    fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

const BROWSERS: &str = r#"[
  {"instance_id":"85d07e03","browser_name":"chrome","browser_version":"155.0.0.0",
   "extension_version":"0.3.3","label":"Work Profile","session_count":2,
   "connected_at_ms":1792111404644,"version_skew":false,
   "extension_protocol_version":"1.3","unresponsive":false,"future_field":{"a":1}},
  {"instance_id":"f81ac95e","browser_name":"chrome","label":"Personal","unresponsive":true,"version_skew":true}
]"#;

const STATUS: &str = r#"{"daemon_version":"0.3.3","protocol_version":"1.3","pid":4242,
 "uptime_secs":99,"ws_port":52800,"sock_path":"/x","browsers":[],"sessions":[{"id":"s"}],
 "version_skew_browsers":[]}"#;

const DOCTOR: &str = "ok    bsk home writable                /x/.bsk\n\
ok    daemon running                   pid 1 at ws://127.0.0.1:52800\n\
warn  auto-update                      last check failed\n\
fail  extension connected              0 browser(s) connected\n";

fn skill_src(dir: &Path) -> PathBuf {
    let s = dir.join("src-skill");
    fs::create_dir_all(s.join("references")).unwrap();
    fs::write(s.join("SKILL.md"), "# browser skill v1\n").unwrap();
    fs::write(s.join("references/tabs.md"), "tabs\n").unwrap();
    s
}

fn set_mtime(p: &Path, t: SystemTime) {
    fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

// ---------- parsing ----------

#[test]
fn parses_the_browsers_list_and_ignores_what_it_does_not_know() {
    let b = browser::parse_browsers(BROWSERS);
    assert_eq!(b.len(), 2);
    assert_eq!(b[0].instance_id, "85d07e03");
    assert_eq!(b[0].label, "Work Profile");
    assert_eq!(b[0].session_count, 2);
    assert_eq!(b[0].extension_version, "0.3.3");
    assert!(!b[0].unresponsive && !b[0].version_skew);
    // Missing fields default; flags are read.
    assert_eq!(b[1].browser_version, "");
    assert_eq!(b[1].session_count, 0);
    assert!(b[1].unresponsive && b[1].version_skew);
    // An object with a `browsers` list works too.
    let wrapped = format!("{{\"browsers\": {BROWSERS}}}");
    assert_eq!(browser::parse_browsers(&wrapped).len(), 2);
}

#[test]
fn browsers_json_garbage_and_empty_give_nothing() {
    for t in [
        "",
        "   ",
        "not json",
        "{}",
        "[]",
        "[1, 2, \"x\"]",
        "null",
        "[{\"label\":\"no id\"}]",
        "{\"browsers\": 7}",
    ] {
        assert!(browser::parse_browsers(t).is_empty(), "{t:?}");
    }
}

#[test]
fn parses_status_and_refuses_what_is_not_a_status() {
    let s = browser::parse_status(STATUS).unwrap();
    assert_eq!(
        (
            s.version.as_str(),
            s.pid,
            s.ws_port,
            s.uptime_secs,
            s.sessions
        ),
        ("0.3.3", 4242, 52800, 99, 1)
    );
    assert!(browser::parse_status("").is_none());
    assert!(browser::parse_status("oops").is_none());
    assert!(browser::parse_status("{\"hello\":1}").is_none());
    assert!(browser::parse_status("[]").is_none());
}

#[test]
fn parses_the_doctor_text() {
    let d = browser::parse_doctor(DOCTOR);
    assert_eq!(d.lines.len(), 4);
    assert_eq!((d.ok(), d.warn(), d.fail()), (2, 1, 1));
    assert_eq!(d.summary(), "2 ok, 1 warning, 1 failed");
    assert_eq!(d.lines[1].name, "daemon running");
    assert!(d.lines[1].detail.starts_with("pid 1"));
    assert_eq!(d.problems().len(), 2);
    assert!(browser::parse_doctor("garbage\n\nmore garbage")
        .lines
        .is_empty());
    assert_eq!(browser::parse_doctor("").summary(), "no result");
}

#[test]
fn parses_chrome_local_state_profiles_in_order() {
    let t = r#"{"profile":{"info_cache":{
      "Profile 2":{"name":"Acme","user_name":"a@x.com","gaia_name":"A","avatar_icon":"zz"},
      "Default":{"name":"Personal","user_name":"me@x.com"},
      "Profile 10":{"name":"Ten"},
      "Profile 1":{"name":"One","gaia_name":"G"}}},"other":{"secret":"no"}}"#;
    let p = browser::parse_local_state(t);
    let dirs: Vec<&str> = p.iter().map(|p| p.dir.as_str()).collect();
    assert_eq!(dirs, ["Default", "Profile 1", "Profile 2", "Profile 10"]);
    assert_eq!(p[0].name, "Personal");
    assert_eq!(p[0].user_name, "me@x.com");
    assert_eq!(p[2].gaia_name, "A");
    assert_eq!(p[1].display(), "One");
    assert!(browser::parse_local_state("").is_empty());
    assert!(browser::parse_local_state("{}").is_empty());
    assert!(browser::parse_local_state("{\"profile\":{}}").is_empty());
}

#[test]
fn reads_the_extension_state_from_secure_preferences() {
    let id = browser::EXTENSION_ID;
    let mk = |e: Value| json!({"extensions": {"settings": {id: e}}}).to_string();
    assert_eq!(
        browser::parse_ext_pref(&mk(json!({"state": 1})), id),
        ExtPref::Enabled
    );
    assert_eq!(
        browser::parse_ext_pref(&mk(json!({"state": 0})), id),
        ExtPref::Disabled
    );
    assert_eq!(
        browser::parse_ext_pref(&mk(json!({"state": 1, "disable_reasons": 1})), id),
        ExtPref::Disabled
    );
    assert_eq!(
        browser::parse_ext_pref(&mk(json!({"state": 1, "disable_reasons": [2]})), id),
        ExtPref::Disabled
    );
    assert_eq!(
        browser::parse_ext_pref(&mk(json!({"state": 1, "disable_reasons": []})), id),
        ExtPref::Enabled
    );
    let other = json!({"extensions": {"settings": {"zzz": {"state": 1}}}}).to_string();
    assert_eq!(browser::parse_ext_pref(&other, id), ExtPref::Missing);
    assert_eq!(browser::parse_ext_pref("garbage", id), ExtPref::Missing);
}

#[test]
fn finds_the_extension_per_chrome_profile_in_a_fixture() {
    let root = tmp("chrome");
    let id = browser::EXTENSION_ID;
    fs::write(
        root.join("Local State"),
        r#"{"profile":{"info_cache":{"Default":{"name":"Me"},"Profile 1":{"name":"Work"},"Profile 2":{"name":"Bare"}}}}"#,
    )
    .unwrap();
    // Default: files and enabled.
    fs::create_dir_all(root.join("Default/Extensions").join(id).join("0.3.3_0")).unwrap();
    fs::write(
        root.join("Default/Secure Preferences"),
        json!({"extensions": {"settings": {id: {"state": 1}}}}).to_string(),
    )
    .unwrap();
    // Profile 1: files, disabled.
    fs::create_dir_all(root.join("Profile 1/Extensions").join(id)).unwrap();
    fs::write(
        root.join("Profile 1/Secure Preferences"),
        json!({"extensions": {"settings": {id: {"state": 0}}}}).to_string(),
    )
    .unwrap();
    // Profile 2: nothing.
    fs::create_dir_all(root.join("Profile 2")).unwrap();
    let v = browser::chrome_extensions(&root);
    assert_eq!(v.len(), 3);
    assert_eq!(v[0].word(), "installed, enabled");
    assert_eq!(v[1].word(), "installed, disabled");
    assert_eq!(v[2].word(), "not installed");
    assert!(!v[2].present());
    // No Local State: no profiles, no error.
    assert!(browser::chrome_extensions(&tmp("chrome-none")).is_empty());
}

// ---------- the look ----------

#[test]
fn bsk_absent_is_reported_without_running_anything() {
    let d = tmp("absent");
    let env = Env {
        bin_dirs: vec![d.join("none"), d.join("bin")],
        chrome_root: d.join("chrome"),
        timeout: Duration::from_secs(2),
        skill_sources: vec![],
    };
    assert!(browser::find_bsk(&env.bin_dirs).is_none());
    let s = browser::collect(&env, &[]);
    assert_eq!(s.daemon, Daemon::NotInstalled);
    assert!(s.version.is_none() && s.browsers.is_empty() && s.doctor.is_none());
    assert_eq!(s.to_json()["bsk"]["installed"], json!(false));
}

#[cfg(unix)]
#[test]
fn a_hung_bsk_is_killed_at_the_timeout_and_never_auto_starts() {
    let d = tmp("timeout");
    let bin = fake_bsk(&d.join("bin"), "sleep 30");
    let t0 = std::time::Instant::now();
    let r = browser::run_bsk(&bin, &["status", "--json"], Duration::from_millis(300));
    assert_eq!(r, Err(browser::RunErr::Timeout));
    assert!(t0.elapsed() < Duration::from_secs(5));
    // The daemon is never auto started.
    let bin = fake_bsk(&d.join("bin2"), "echo \"auto=$BSK_AUTO_START\"");
    let (code, out) = browser::run_bsk(&bin, &["status"], Duration::from_secs(5)).unwrap();
    assert_eq!((code, out.trim()), (0, "auto=0"));
    // A program that cannot start.
    assert!(matches!(
        browser::run_bsk(&d.join("missing"), &[], Duration::from_secs(1)),
        Err(browser::RunErr::Spawn(_))
    ));
}

#[cfg(unix)]
fn env_with(d: &Path, body: &str, ms: u64) -> Env {
    fake_bsk(&d.join("bin"), body);
    Env {
        bin_dirs: vec![d.join("bin")],
        chrome_root: d.join("chrome"),
        timeout: Duration::from_millis(ms),
        skill_sources: vec![],
    }
}

#[cfg(unix)]
#[test]
fn the_look_gathers_daemon_browsers_doctor_and_harnesses() {
    let d = tmp("look");
    let body = format!(
        "case \"$1\" in\n--version) echo 'bsk 0.3.3';;\nstatus) cat <<'EOF'\n{STATUS}\nEOF\n;;\nbrowsers) cat <<'EOF'\n{BROWSERS}\nEOF\n;;\ndoctor) cat <<'EOF'\n{DOCTOR}\nEOF\n;;\ninstall-skill) echo '[{{\"id\":\"claude-code\",\"skills_dir\":\"/x\",\"detected\":true,\"installed\":true}}]';;\nesac"
    );
    let env = env_with(&d, &body, 5000);
    let acct = browser::AccountRef {
        number: 1,
        label: "Work".into(),
        harness: "claude".into(),
        dir: d.join("acct1"),
    };
    fs::create_dir_all(browser::skill_dir(&acct.dir)).unwrap();
    fs::write(browser::skill_dir(&acct.dir).join("SKILL.md"), "x").unwrap();
    let s = browser::collect(&env, &[acct]);
    assert_eq!(s.version.as_deref(), Some("0.3.3"));
    assert!(matches!(&s.daemon, Daemon::Running(i) if i.pid == 4242));
    assert_eq!(s.browsers.len(), 2);
    assert_eq!(s.doctor.as_ref().unwrap().fail(), 1);
    assert!(s.harnesses[0].installed);
    assert!(s.accounts[0].installed);
    let j = s.to_json();
    assert_eq!(j["daemon"]["state"], "running");
    assert_eq!(j["browsers"][0]["label"], "Work Profile");
    assert_eq!(j["godterm_accounts"][0]["skill_installed"], true);
    assert!(s.browsers_json()["note"]
        .as_str()
        .unwrap()
        .contains("not the Chrome profile"));
}

#[cfg(unix)]
#[test]
fn a_stopped_daemon_is_stopped_and_a_slow_one_is_not_answering() {
    let d = tmp("stopped");
    let env = env_with(
        &d,
        "case \"$1\" in --version) echo 0.3.3;; *) exit 1;; esac",
        3000,
    );
    let s = browser::collect(&env, &[]);
    assert_eq!(s.daemon, Daemon::Stopped);
    assert!(s.browsers.is_empty());
    let d = tmp("slow");
    let env = env_with(
        &d,
        "case \"$1\" in --version) echo 0.3.3;; *) sleep 20;; esac",
        300,
    );
    let s = browser::collect(&env, &[]);
    assert_eq!(s.daemon, Daemon::Unresponsive);
    // Garbage from a "running" daemon is unknown, not a crash.
    let d = tmp("garbage");
    let env = env_with(&d, "echo 'what'", 3000);
    assert!(matches!(
        browser::collect(&env, &[]).daemon,
        Daemon::Unknown(_)
    ));
}

// ---------- the skill copy ----------

#[test]
fn copies_the_skill_fresh_and_recognizes_a_current_copy() {
    let d = tmp("fresh");
    let src = skill_src(&d);
    let dest = d.join("acct/skills/browser-skill");
    assert_eq!(skill::plan(&src, &dest, false).unwrap(), Action::Install);
    skill::install(&src, &dest).unwrap();
    assert_eq!(
        fs::read_to_string(dest.join("SKILL.md")).unwrap(),
        "# browser skill v1\n"
    );
    assert_eq!(
        fs::read_to_string(dest.join("references/tabs.md")).unwrap(),
        "tabs\n"
    );
    // A copy, not a link.
    assert!(!fs::symlink_metadata(&dest)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(skill::plan(&src, &dest, false).unwrap(), Action::Current);
    // No leftovers beside it.
    let names: Vec<_> = fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["browser-skill"]);
    // No source skill: refused.
    assert!(skill::plan(&d.join("nothing"), &dest, false).is_err());
    assert!(skill::install(&d.join("nothing"), &d.join("x/browser-skill")).is_err());
}

#[test]
fn a_newer_copy_is_kept_unless_forced_and_an_older_one_is_updated() {
    let d = tmp("newer");
    let src = skill_src(&d);
    let dest = d.join("acct/skills/browser-skill");
    skill::install(&src, &dest).unwrap();
    // The account's copy was edited later than the source.
    fs::write(dest.join("SKILL.md"), "# my edit\n").unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    for f in ["SKILL.md", "references/tabs.md"] {
        set_mtime(&src.join(f), old);
    }
    assert_eq!(skill::plan(&src, &dest, false).unwrap(), Action::KeptNewer);
    assert_eq!(skill::plan(&src, &dest, true).unwrap(), Action::Update);
    skill::install(&src, &dest).unwrap();
    assert_eq!(
        fs::read_to_string(dest.join("SKILL.md")).unwrap(),
        "# browser skill v1\n"
    );
    // The other way: the source is newer, so a plain run updates.
    fs::write(src.join("SKILL.md"), "# v2\n").unwrap();
    set_mtime(&dest.join("SKILL.md"), old);
    set_mtime(&dest.join("references/tabs.md"), old);
    assert_eq!(skill::plan(&src, &dest, false).unwrap(), Action::Update);
    skill::install(&src, &dest).unwrap();
    assert_eq!(fs::read_to_string(dest.join("SKILL.md")).unwrap(), "# v2\n");
}

#[cfg(unix)]
#[test]
fn a_failed_copy_leaves_the_old_copy_and_no_temp_folder() {
    use std::os::unix::fs::PermissionsExt;
    let d = tmp("atomic");
    let src = skill_src(&d);
    let dest = d.join("acct/skills/browser-skill");
    skill::install(&src, &dest).unwrap();
    fs::write(src.join("SKILL.md"), "# new\n").unwrap();
    let bad = src.join("references/secret.md");
    fs::write(&bad, "x").unwrap();
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o000)).unwrap();
    let r = skill::install(&src, &dest);
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(r.is_err(), "{r:?}");
    assert_eq!(
        fs::read_to_string(dest.join("SKILL.md")).unwrap(),
        "# browser skill v1\n"
    );
    let names: Vec<_> = fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["browser-skill"], "no temp or old folder left");
}

// ---------- the settings.json merge ----------

#[test]
fn adds_the_permission_and_keeps_every_other_key() {
    let d = tmp("merge");
    let p = d.join("settings.json");
    fs::write(
        &p,
        r#"{"model":"opus","hooks":{"Stop":[{"x":1}]},"permissions":{"allow":["Bash(git:*)"],"deny":["Bash(rm:*)"],"defaultMode":"plan"}}"#,
    )
    .unwrap();
    let r = skill::allow_rule(&p, BSK_RULE).unwrap();
    let Merge::Added { backup: Some(b) } = r else {
        panic!("{r:?}");
    };
    let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(
        v["permissions"]["allow"],
        json!(["Bash(git:*)", "Bash(bsk:*)"])
    );
    assert_eq!(v["permissions"]["deny"], json!(["Bash(rm:*)"]));
    assert_eq!(v["permissions"]["defaultMode"], "plan");
    assert_eq!(v["model"], "opus");
    assert_eq!(v["hooks"]["Stop"][0]["x"], 1);
    // The backup is the file as it was.
    let old: Value = serde_json::from_str(&fs::read_to_string(&b).unwrap()).unwrap();
    assert_eq!(old["permissions"]["allow"], json!(["Bash(git:*)"]));
    assert!(b
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains("godterm-bak"));
}

#[test]
fn the_merge_is_idempotent_and_makes_a_missing_file() {
    let d = tmp("idem");
    let p = d.join("sub/settings.json");
    // Missing: created, no backup.
    assert_eq!(
        skill::allow_rule(&p, BSK_RULE).unwrap(),
        Merge::Added { backup: None }
    );
    let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(v["permissions"]["allow"], json!(["Bash(bsk:*)"]));
    // Again: nothing changes, no new backup.
    let before = fs::read_to_string(&p).unwrap();
    assert_eq!(
        skill::allow_rule(&p, BSK_RULE).unwrap(),
        Merge::AlreadyAllowed
    );
    assert_eq!(fs::read_to_string(&p).unwrap(), before);
    let n = fs::read_dir(p.parent().unwrap()).unwrap().count();
    assert_eq!(n, 1, "only settings.json");
    // An empty file counts as {}.
    let e = d.join("empty.json");
    fs::write(&e, "").unwrap();
    assert!(matches!(
        skill::allow_rule(&e, BSK_RULE).unwrap(),
        Merge::Added { .. }
    ));
}

#[test]
fn invalid_json_or_the_wrong_shape_is_refused_and_left_alone() {
    let d = tmp("refuse");
    for (name, text) in [
        ("bad.json", "{ not json"),
        ("array.json", "[1,2]"),
        ("perm.json", r#"{"permissions": "all"}"#),
        ("allow.json", r#"{"permissions": {"allow": "Bash"}}"#),
    ] {
        let p = d.join(name);
        fs::write(&p, text).unwrap();
        let r = skill::allow_rule(&p, BSK_RULE);
        assert!(r.is_err(), "{name}: {r:?}");
        assert_eq!(fs::read_to_string(&p).unwrap(), text, "{name} untouched");
    }
    // And no backup or temp file was left for them.
    assert_eq!(fs::read_dir(&d).unwrap().count(), 4);
}

// ---------- the assistant's tools ----------

fn test_app(tag: &str) -> (crate::app::App, PathBuf) {
    let home =
        std::env::temp_dir().join(format!("godterm-browser-app-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    assert!(app.cfg.accounts.len() >= 3);
    app.cfg.accounts.truncate(3);
    for (i, a) in app.cfg.accounts.iter_mut().enumerate() {
        let dir = home.join(format!("acct{}", i + 1));
        fs::create_dir_all(&dir).unwrap();
        a.config_dir = Some(dir.display().to_string());
    }
    app.cfg.accounts[2].harness = "grok".into();
    let src = skill_src(&home);
    app.browser.env = Some(Env {
        bin_dirs: vec![],
        chrome_root: home.join("chrome"),
        timeout: Duration::from_secs(2),
        skill_sources: vec![src],
    });
    (app, home)
}

fn user_says(app: &mut crate::app::App, said: &str) {
    app.assistant_turn += 1;
    app.assistant.last_user = said.into();
    app.assistant.turn_reads = 0;
}

fn acct(home: &Path, n: usize) -> PathBuf {
    home.join(format!("acct{n}"))
}

#[test]
fn installing_the_skill_needs_a_yes_and_reports_each_account() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("install");
    // Account 1 already has a settings.json with other keys.
    fs::write(acct(&home, 1).join("settings.json"), r#"{"model":"opus"}"#).unwrap();
    user_says(&mut app, "install the browser skill everywhere");
    let v = app.control_call("install_browser_skill", &json!({"_client": "brain"}));
    assert_eq!(v["needs_confirmation"], true, "{v}");
    let q = v["question"].as_str().unwrap();
    assert!(
        q.contains("Bash(bsk:*)") && q.contains("browser skill"),
        "{q}"
    );
    // Nothing before the yes.
    for n in 1..=3 {
        assert!(!acct(&home, n).join("skills").exists(), "account {n}");
    }
    assert_eq!(
        fs::read_to_string(acct(&home, 1).join("settings.json")).unwrap(),
        r#"{"model":"opus"}"#
    );
    // A yes in the same turn is refused; a clear yes later runs it.
    let tok = v["token"].as_str().unwrap().to_string();
    let same = app.control_call(
        "install_browser_skill",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert_eq!(same["ok"], false, "{same}");
    user_says(&mut app, "yes");
    let r = app.control_call(
        "install_browser_skill",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let rows = r["result"]["accounts"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["skill"], "installed");
    assert!(rows[0]["permission"]
        .as_str()
        .unwrap()
        .starts_with("added (backup"));
    // The Grok account gets the skill only.
    assert_eq!(rows[2]["skill"], "installed");
    assert_eq!(rows[2]["permission"], "not requested");
    for n in 1..=3 {
        assert!(
            acct(&home, n)
                .join("skills/browser-skill/SKILL.md")
                .is_file(),
            "account {n}"
        );
    }
    let s1: Value =
        serde_json::from_str(&fs::read_to_string(acct(&home, 1).join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(s1["model"], "opus");
    assert_eq!(s1["permissions"]["allow"], json!(["Bash(bsk:*)"]));
    assert!(!acct(&home, 3).join("settings.json").exists());
    // Run again: nothing to change, no question.
    user_says(&mut app, "install the browser skill again");
    let again = app.control_call("install_browser_skill", &json!({"_client": "brain"}));
    assert!(again["needs_confirmation"].is_null(), "{again}");
    assert_eq!(again["result"]["accounts"][0]["skill"], "already current");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_declined_or_unsourced_install_changes_nothing() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("decline");
    user_says(&mut app, "install the browser skill on account 2");
    let v = app.control_call(
        "install_browser_skill",
        &json!({"_client": "brain", "account": 2, "allow_bsk": false}),
    );
    let tok = v["token"].as_str().unwrap().to_string();
    user_says(&mut app, "no");
    let r = app.control_call(
        "install_browser_skill",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert_eq!(r["result"]["cancelled"], true, "{r}");
    assert!(!acct(&home, 2).join("skills").exists());
    // No source skill on this machine: an error that says bsk is not installed by GodTerm.
    app.browser.env.as_mut().unwrap().skill_sources = vec![home.join("nope")];
    user_says(&mut app, "install the browser skill");
    let e = app.control_call("install_browser_skill", &json!({"_client": "brain"}));
    assert_eq!(e["ok"], false);
    assert!(
        e["error"]
            .as_str()
            .unwrap()
            .contains("does not install BrowserSkill"),
        "{e}"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn list_browsers_answers_from_the_last_look_and_checks_in_the_background() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("list");
    // No look yet: "checking", and a look starts (no bsk here: not installed).
    let v = app.control_call("list_browsers", &json!({"_client": "brain"}));
    assert_eq!(v["result"]["state"], "checking", "{v}");
    let t0 = std::time::Instant::now();
    while app.browser.snapshot().is_none() && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let v = app.control_call("browser_status", &json!({"_client": "brain"}));
    assert_eq!(v["result"]["daemon"]["state"], "bsk not installed", "{v}");
    assert_eq!(v["result"]["godterm_accounts"].as_array().unwrap().len(), 3);
    // A given look is what list_browsers reports.
    let mut s = app.browser.snapshot().unwrap();
    s.daemon = Daemon::Stopped;
    s.browsers = browser::parse_browsers(BROWSERS);
    app.browser.set(s);
    let v = app.control_call("list_browsers", &json!({"_client": "brain"}));
    assert_eq!(v["result"]["count"], 2, "{v}");
    assert_eq!(v["result"]["browsers"][1]["instance_id"], "f81ac95e");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn the_browser_tools_are_declared_and_the_reads_are_read_only() {
    for name in ["list_browsers", "browser_status", "install_browser_skill"] {
        let t = crate::control::TOOLS
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name}"));
        assert!(!t.description.is_empty());
        let s = (t.schema)();
        assert_eq!(s["type"], "object", "{name}");
        assert!(s["properties"].is_object(), "{name}");
    }
    let props = |n: &str| {
        let t = crate::control::TOOLS.iter().find(|t| t.name == n).unwrap();
        (t.schema)()["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(props("list_browsers").is_empty() && props("browser_status").is_empty());
    let p = props("install_browser_skill");
    for k in [
        "account",
        "force",
        "allow_bsk",
        "confirm_token",
        "reissue_token",
    ] {
        assert!(p.contains(&k.to_string()), "{k}");
    }
    assert!(crate::control::READ_ONLY.contains(&"list_browsers"));
    assert!(crate::control::READ_ONLY.contains(&"browser_status"));
    assert!(!crate::control::READ_ONLY.contains(&"install_browser_skill"));
    assert!(crate::app_admin::WRITE_TOOLS.contains(&"install_browser_skill"));
}

#[test]
fn tabs_get_the_two_reads_and_never_the_install() {
    use crate::tab_jobs::{self, HINT, TAB_TOOLS};
    assert!(TAB_TOOLS.contains(&"list_browsers") && TAB_TOOLS.contains(&"browser_status"));
    assert!(!TAB_TOOLS.contains(&"install_browser_skill"));
    // The five job tools are unchanged.
    for t in [
        "create_job",
        "list_jobs",
        "update_job",
        "delete_job",
        "adopt_loop",
    ] {
        assert!(TAB_TOOLS.contains(&t), "{t}");
    }
    assert!(
        HINT.contains("`bsk browsers --json`")
            && HINT.contains("`bsk session start --browser <id>`")
    );
    assert!(HINT.contains("GodTerm's list_browsers shows what is connected."));
    let shared = "s";
    let req = |tool: &str| json!({"tool": tool, "tab": 3, "token": tab_jobs::tab_token(shared, 3)});
    assert_eq!(tab_jobs::authorize(shared, &req("list_browsers")), Ok(3));
    assert_eq!(tab_jobs::authorize(shared, &req("browser_status")), Ok(3));
    let e = tab_jobs::authorize(shared, &req("install_browser_skill")).unwrap_err();
    assert!(e.contains("not available to a tab"), "{e}");
    let list = tab_jobs::tab_tool_list();
    let names: Vec<&str> = list.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"list_browsers") && !names.contains(&"install_browser_skill"));
}

#[test]
fn a_tab_reads_the_browsers_but_cannot_pass_arguments_or_install() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("tabread");
    let t = crate::jobs_tests::running(&mut app, 0, "web", "sess-w");
    let uid = app.panes[0].tabs[t].uid;
    let call = |app: &mut crate::app::App, tool: &str, mut a: Value| {
        a["_client"] = json!(format!("tab:{uid}"));
        app.control_call(tool, &a)
    };
    let v = call(&mut app, "list_browsers", json!({}));
    assert_eq!(v["ok"], true, "{v}");
    let v = call(&mut app, "browser_status", json!({}));
    assert_eq!(v["ok"], true, "{v}");
    let v = call(&mut app, "list_browsers", json!({"tab": "t999"}));
    assert_eq!(v["ok"], false, "{v}");
    let v = call(&mut app, "browser_status", json!({"confirm_token": "x"}));
    assert_eq!(v["ok"], false, "{v}");
    let v = call(&mut app, "install_browser_skill", json!({}));
    assert_eq!(v["ok"], false);
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("not available to a tab"),
        "{v}"
    );
    for n in 1..=3 {
        assert!(!acct(&home, n).join("skills").exists());
    }
    let _ = fs::remove_dir_all(home);
}
