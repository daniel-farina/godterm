#![cfg_attr(not(unix), allow(dead_code))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::browser::ChromeProfile;
use crate::browser_view::*;
use crate::web::tap::{Out, Outbox};

pub(crate) fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("godterm-bview-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
    b
}

#[cfg(unix)]
pub(crate) fn fake(d: &Path, browsers: &str, sessions: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    fs::write(d.join("browsers.json"), browsers).unwrap();
    fs::write(d.join("sessions.json"), sessions).unwrap();
    fs::write(d.join("fix.png"), png(1280, 800)).unwrap();
    fs::write(
        d.join("eval.json"),
        r#"{"ok":true,"result":{"mode":"native","tag":"A","w":1280,"h":800}}"#,
    )
    .unwrap();
    let body = format!(
        r#"#!/bin/sh
D='{d}'
echo "$*" >> "$D/log"
case "$1" in
  browsers) cat "$D/browsers.json";;
  session)
    case "$2" in
      list) cat "$D/sessions.json";;
      start) echo '{{"ok":true,"session_id":"s-new"}}'
             printf '[{{"session_id":"s-new","browser_instance_id":"aaa"}}]' > "$D/sessions.json";;
    esac;;
  tab) echo '[{{"tab_id":7,"url":"https://example.com/","title":"Example Domain","active":true}}]';;
  screenshot)
    while [ $# -gt 0 ]; do
      if [ "$1" = "--out" ]; then cp "$D/fix.png" "$2"; fi
      shift
    done
    echo '{{"ok":true}}';;
  evaluate) cat "$D/eval.json";;
  *) echo '{{"ok":true}}';;
esac
"#,
        d = d.display()
    );
    let p = d.join("bsk");
    fs::write(&p, body).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

pub(crate) const BROWSERS: &str =
    r#"[{"instance_id":"aaa","label":"Work"},{"instance_id":"bbb","label":"Personal"}]"#;

pub(crate) fn ctx(d: &Path, bin: PathBuf, uid: u64) -> Ctx {
    Ctx {
        uid,
        bin: Some(bin),
        profile: Some(ChromeProfile {
            dir: "Default".into(),
            name: "Work".into(),
            ..Default::default()
        }),
        enabled_profiles: 2,
        this_enabled: true,
        transcript: None,
        also: vec![],
        links: Default::default(),
        shared: false,
        tmp: d.join("pics"),
        sips: None,
    }
}

fn log(d: &Path) -> String {
    fs::read_to_string(d.join("log")).unwrap_or_default()
}

fn next(out: &Outbox, op: &str, wait: Duration) -> Option<Value> {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        if let Some(Out::Text(s)) = out.pop(Duration::from_millis(50)) {
            let v: Value = serde_json::from_str(&s).unwrap();
            if v["t"] == "bview" && v["op"] == op {
                return Some(v);
            }
        }
    }
    None
}

fn picture(out: &Outbox, wait: Duration) -> Option<(u64, Vec<u8>)> {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        if let Some(Out::Picture(b)) = out.pop(Duration::from_millis(50)) {
            let (k, uid, p) = crate::web::ws::unframe(&b).unwrap();
            assert_eq!(k, KIND_PICTURE);
            return Some((uid, p.to_vec()));
        }
    }
    None
}

fn wait_until(mut f: impl FnMut() -> bool, wait: Duration) -> bool {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    f()
}

#[test]
fn parses_sessions_tabs_and_sizes() {
    let s = parse_sessions(
        r#"[{"session_id":"s1","browser_instance_id":"aaa","agent_window_id":3,"created_at_ms":5,"name":"x"},
            {"browser_instance_id":"aaa"}, 7]"#,
    );
    assert_eq!(s.len(), 1);
    assert_eq!(
        (s[0].id.as_str(), s[0].browser.as_str(), s[0].created_at_ms),
        ("s1", "aaa", 5)
    );
    assert_eq!(
        parse_sessions(r#"{"sessions":[{"id":"s2","browser":"b"}]}"#)[0].id,
        "s2"
    );
    assert!(parse_sessions("not json").is_empty());

    let t = parse_tabs(
        r#"{"tabs":[{"tab_id":1,"url":"a","title":"A"},{"tab_id":2,"url":"b","title":"B","active":true}]}"#,
    );
    assert_eq!(t[0].id, "1");
    assert_eq!(current_tab(&t).unwrap().url, "b");
    assert_eq!(
        current_tab(&parse_tabs(r#"[{"id":"x","url":"u"}]"#))
            .unwrap()
            .url,
        "u"
    );
    assert!(current_tab(&[]).is_none());

    assert_eq!(image_size(&png(1280, 800)), Some((1280, 800)));

    let mut j = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0];
    j.extend_from_slice(&[0xff, 0xc0, 0, 11, 8, 0x01, 0x90, 0x02, 0x80, 3, 0, 0]);
    assert_eq!(image_size(&j), Some((640, 400)));
    assert_eq!(image_size(b"GIF89a"), None);
}

#[test]
fn finds_the_agents_session_in_its_transcript() {
    let call = |cmd: &str| {
        json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "input": {"command": cmd}}]}}).to_string()
    };
    let out = json!({"type": "user", "message": {"content": [{"type": "tool_result",
        "content": "{\n  \"session_id\": \"s-from-start\",\n  \"agent_window_id\": 4\n}"}]}})
    .to_string();
    let tail = [call("bsk session start --browser aaa --json"), out.clone()].join("\n");
    assert_eq!(last_session(&tail).as_deref(), Some("s-from-start"));
    let tail = [
        out,
        call("bsk navigate https://x.test --session s-later"),
        call("ls"),
    ]
    .join("\n");
    assert_eq!(last_session(&tail).as_deref(), Some("s-later"));

    assert_eq!(
        last_session(&call("bsk click @e3 --session \"$SID\"")),
        None
    );
    assert_eq!(last_session(&call("echo --session nope")), None);
    assert_eq!(
        last_session(&call("bsk snapshot --session=s9 --json")).as_deref(),
        Some("s9")
    );
    assert!(!valid_id("a b") && !valid_id("") && !valid_id("$X") && valid_id("8c1f-22:ab_c.d"));
}

#[test]
fn chooses_only_sessions_on_the_tabs_own_browser() {
    let s = |id: &str, b: &str| BskSession {
        id: id.into(),
        browser: b.into(),
        ..Default::default()
    };
    let mine = vec!["aaa".to_string()];
    let all = vec![s("other", "bbb"), s("a1", "aaa")];
    assert_eq!(
        choose(&all, &mine, None, None, false),
        Choice::Session("a1".into(), "the only one")
    );

    assert_eq!(
        choose(&all, &mine, Some("other"), Some("other"), false),
        Choice::Session("a1".into(), "the only one")
    );
    assert_eq!(
        choose(&[s("other", "bbb")], &mine, None, Some("other"), false),
        Choice::NoSession
    );
    let two = vec![s("a1", "aaa"), s("a2", "aaa")];
    assert_eq!(choose(&two, &mine, None, None, false), Choice::Unsure(2));
    assert_eq!(
        choose(&two, &mine, None, Some("a2"), false),
        Choice::Session("a2".into(), "the agent's")
    );
    assert_eq!(
        choose(&two, &mine, Some("a1"), Some("a2"), false),
        Choice::Session("a1".into(), "started here")
    );

    assert_eq!(
        choose(&[s("a1", "aaa")], &mine, None, None, true),
        Choice::Unsure(1)
    );
    assert_eq!(choose(&[], &mine, None, None, false), Choice::NoSession);
}

#[test]
fn urls_and_keys() {
    assert_eq!(
        clean_url("example.com/a?b").as_deref(),
        Some("https://example.com/a?b")
    );
    assert_eq!(
        clean_url(" http://x.test ").as_deref(),
        Some("http://x.test")
    );
    assert_eq!(
        clean_url("localhost:3000").as_deref(),
        Some("https://localhost:3000")
    );
    for bad in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "chrome://settings",
        "",
        "two words.com",
        "data:text/html,x",
        "a\nb.com",
    ] {
        assert_eq!(clean_url(bad), None, "{bad}");
    }
    assert!(valid_key("Enter") && valid_key("Ctrl+A") && !valid_key("a b") && !valid_key("--x"));
    let js = click_script(0.5, 0.25, "m1");
    assert!(
        js.contains("0.5 * innerWidth") && js.contains("0.25 * innerHeight") && js.contains("'m1'")
    );
}

#[test]
fn a_picture_waits_for_the_one_queued() {
    let out = Outbox::new();
    assert!(out.picture(vec![5, 0, 0, 0, 1, 9]));
    assert!(
        !out.picture(vec![5, 0, 0, 0, 1, 8]),
        "a second picture is dropped while one is queued"
    );
    out.text("{}".into());
    assert!(matches!(out.pop(Duration::ZERO), Some(Out::Picture(_))));
    assert!(out.picture(vec![5, 0, 0, 0, 1, 7]));
    assert_eq!(out.queued().1, 2);
}

#[cfg(unix)]
#[test]
fn open_streams_pictures_and_meta_until_closed() {
    let d = tmp("stream");
    let bin = fake(
        &d,
        BROWSERS,
        r#"[{"session_id":"s-stream","browser_instance_id":"aaa"},{"session_id":"s-theirs","browser_instance_id":"bbb"}]"#,
    );
    let c = ctx(&d, bin, 41);
    let out = Outbox::new();
    handle(&c, 1, &out, &json!({"op": "open", "id": "q1", "tab": 41}));
    let r = next(&out, "open", Duration::from_secs(5)).expect("open answered");
    assert_eq!(
        (r["ok"].as_bool(), r["state"].as_str(), r["id"].as_str()),
        (Some(true), Some("live"), Some("q1"))
    );
    assert!(streaming("s-stream"));
    let m = next(&out, "meta", Duration::from_secs(5)).expect("meta");
    assert_eq!(
        (m["url"].as_str(), m["title"].as_str(), m["tab"].as_u64()),
        (
            Some("https://example.com/"),
            Some("Example Domain"),
            Some(41)
        )
    );
    let (uid, p) = picture(&out, Duration::from_secs(5)).expect("a picture");
    assert_eq!((uid, image_size(&p)), (41, Some((1280, 800))));

    assert!(picture(&out, Duration::from_secs(5)).is_some());
    let l = log(&d);
    assert!(
        l.contains("screenshot --session s-stream --json --out"),
        "{l}"
    );
    assert!(
        !l.contains("s-theirs") && !l.contains("session start"),
        "{l}"
    );

    handle(&c, 1, &out, &json!({"op": "close", "tab": 41}));
    assert!(
        wait_until(|| !streaming("s-stream"), Duration::from_secs(6)),
        "the loop stops"
    );
    let n = log(&d).matches("screenshot").count();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        log(&d).matches("screenshot").count(),
        n,
        "no captures after close"
    );
    let _ = fs::remove_dir_all(&d);
}

#[cfg(unix)]
#[test]
fn one_loop_for_two_viewers_and_a_closed_socket_leaves() {
    let d = tmp("shared");
    let bin = fake(
        &d,
        BROWSERS,
        r#"[{"session_id":"s-shared","browser_instance_id":"aaa"}]"#,
    );
    let c = ctx(&d, bin, 42);
    let (a, b) = (Outbox::new(), Outbox::new());
    handle(&c, 10, &a, &json!({"op": "open", "tab": 42}));
    handle(&c, 11, &b, &json!({"op": "open", "tab": 42}));
    assert!(
        picture(&a, Duration::from_secs(5)).is_some()
            && picture(&b, Duration::from_secs(5)).is_some()
    );

    let t0 = Instant::now();
    let n0 = log(&d).matches("screenshot").count();
    std::thread::sleep(Duration::from_millis(2000));
    let n = log(&d).matches("screenshot").count() - n0;
    assert!(
        n <= (t0.elapsed().as_millis() / 600) as usize + 1,
        "{n} captures in 2 s"
    );
    a.close();
    b.close();
    assert!(
        wait_until(|| !streaming("s-shared"), Duration::from_secs(6)),
        "closed sockets end the loop"
    );
    let _ = fs::remove_dir_all(&d);
}

#[cfg(unix)]
#[test]
fn no_session_is_never_started_by_itself() {
    let d = tmp("none");
    let bin = fake(
        &d,
        BROWSERS,
        r#"[{"session_id":"s-other","browser_instance_id":"bbb"}]"#,
    );
    let c = ctx(&d, bin.clone(), 43);
    let out = Outbox::new();
    handle(&c, 2, &out, &json!({"op": "open", "tab": 43}));
    let r = next(&out, "open", Duration::from_secs(5)).unwrap();
    assert_eq!(
        (r["state"].as_str(), r["can_start"].as_bool()),
        (Some("none"), Some(true))
    );
    assert!(r["say"]
        .as_str()
        .unwrap()
        .contains("hasn't opened the browser"));

    handle(
        &c,
        2,
        &out,
        &json!({"op": "act", "tab": 43, "do": "reload"}),
    );
    assert_eq!(
        next(&out, "act", Duration::from_secs(5)).unwrap()["ok"],
        false
    );
    let l = log(&d);
    assert!(
        !l.contains("session start") && !l.contains("screenshot") && !l.contains("reload"),
        "{l}"
    );

    handle(&c, 2, &out, &json!({"op": "start", "tab": 43}));
    let r = next(&out, "start", Duration::from_secs(5)).unwrap();
    assert_eq!(
        (r["ok"].as_bool(), r["state"].as_str()),
        (Some(true), Some("live")),
        "{r}"
    );
    assert!(
        log(&d).contains("session start --browser aaa"),
        "{}",
        log(&d)
    );
    assert!(picture(&out, Duration::from_secs(5)).is_some());
    handle(&c, 2, &out, &json!({"op": "close", "tab": 43}));
    assert!(wait_until(|| !streaming("s-new"), Duration::from_secs(6)));

    let mut c2 = ctx(&d, bin, 44);
    c2.profile = None;
    handle(&c2, 2, &out, &json!({"op": "open", "tab": 44}));
    assert_eq!(
        next(&out, "open", Duration::from_secs(5)).unwrap()["state"],
        "no_browser"
    );
    c2.bin = None;
    c2.profile = c.profile.clone();
    handle(&c2, 2, &out, &json!({"op": "open", "tab": 44}));
    assert_eq!(
        next(&out, "open", Duration::from_secs(5)).unwrap()["state"],
        "no_bsk"
    );
    let _ = fs::remove_dir_all(&d);
}

#[cfg(unix)]
#[test]
fn a_browser_two_tabs_share_needs_the_transcript() {
    let d = tmp("sharedprof");
    let bin = fake(
        &d,
        BROWSERS,
        r#"[{"session_id":"s-a","browser_instance_id":"aaa"}]"#,
    );
    let mut c = ctx(&d, bin, 45);
    c.shared = true;
    let out = Outbox::new();
    handle(&c, 3, &out, &json!({"op": "open", "tab": 45}));
    let r = next(&out, "open", Duration::from_secs(5)).unwrap();
    assert_eq!(
        (r["state"].as_str(), r["can_start"].as_bool()),
        (Some("unsure"), Some(false))
    );
    let t = d.join("t.jsonl");
    fs::write(
        &t,
        json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "input": {"command": "bsk observe --session s-a --json"}}]}}).to_string() + "\n",
    )
    .unwrap();
    c.transcript = Some(t);
    handle(&c, 3, &out, &json!({"op": "open", "tab": 45}));
    assert_eq!(
        next(&out, "open", Duration::from_secs(5)).unwrap()["state"],
        "live"
    );
    handle(&c, 3, &out, &json!({"op": "close", "tab": 45}));
    assert!(wait_until(|| !streaming("s-a"), Duration::from_secs(6)));
    let _ = fs::remove_dir_all(&d);
}

#[cfg(unix)]
#[test]
fn actions_go_to_the_watched_session() {
    let d = tmp("act");
    let bin = fake(
        &d,
        BROWSERS,
        r#"[{"session_id":"s-act","browser_instance_id":"aaa"}]"#,
    );
    let c = ctx(&d, bin, 46);
    let out = Outbox::new();
    handle(&c, 4, &out, &json!({"op": "open", "tab": 46}));
    assert_eq!(
        next(&out, "open", Duration::from_secs(5)).unwrap()["state"],
        "live"
    );
    let act = |v: Value| {
        let mut v = v;
        v["op"] = json!("act");
        v["tab"] = json!(46);
        handle(&c, 4, &out, &v);
        next(&out, "act", Duration::from_secs(5)).unwrap()
    };
    assert_eq!(act(json!({"do": "click", "x": 0.5, "y": 0.25}))["ok"], true);
    let l = log(&d);
    assert!(
        l.contains("evaluate") && l.contains("click --selector [data-godterm-hit="),
        "{l}"
    );

    fs::write(
        d.join("eval.json"),
        r#"{"ok":true,"result":{"mode":"synthetic","w":1000,"h":500}}"#,
    )
    .unwrap();
    let n = log(&d).matches("click --selector").count();
    assert_eq!(act(json!({"do": "click", "x": 0.1, "y": 0.9}))["ok"], true);
    assert_eq!(log(&d).matches("click --selector").count(), n);

    assert_eq!(act(json!({"do": "wheel", "dy": 0.5}))["ok"], true);
    assert!(
        log(&d).contains("wheel --delta-y 250 --delta-x 0 --session s-act"),
        "{}",
        log(&d)
    );
    assert_eq!(act(json!({"do": "click", "x": 2.0, "y": 0.1}))["ok"], false);
    assert_eq!(
        act(json!({"do": "navigate", "url": "javascript:alert(1)"}))["ok"],
        false
    );
    assert_eq!(
        act(json!({"do": "navigate", "url": "example.org"}))["ok"],
        true
    );
    assert!(log(&d).contains("navigate https://example.org --session s-act"));
    for (what, cmd) in [
        ("back", "navigate-back --session s-act"),
        ("forward", "navigate-forward --session s-act"),
        ("reload", "reload --session s-act"),
    ] {
        assert_eq!(act(json!({ "do": what }))["ok"], true);
        assert!(log(&d).contains(cmd), "{what}");
    }
    assert_eq!(act(json!({"do": "key", "key": "Enter"}))["ok"], true);
    assert!(log(&d).contains("press Enter --session s-act"));
    assert_eq!(act(json!({"do": "key", "key": "rm -rf"}))["ok"], false);
    fs::write(
        d.join("eval.json"),
        r#"{"ok":true,"result":{"typed":true}}"#,
    )
    .unwrap();
    assert_eq!(
        act(json!({"do": "type", "text": "hello \"x\""}))["ok"],
        true
    );
    assert!(
        log(&d).contains(r#"insertText', false, "hello \"x\"")"#),
        "{}",
        log(&d)
    );

    handle(
        &c,
        99,
        &out,
        &json!({"op": "act", "tab": 46, "do": "reload"}),
    );
    assert_eq!(
        next(&out, "act", Duration::from_secs(5)).unwrap()["ok"],
        false
    );
    handle(&c, 4, &out, &json!({"op": "close", "tab": 46}));
    assert!(wait_until(|| !streaming("s-act"), Duration::from_secs(6)));
    let _ = fs::remove_dir_all(&d);
}

#[cfg(unix)]
#[test]
fn an_ended_session_says_so_and_stops() {
    let d = tmp("ended");
    let bin = fake(
        &d,
        BROWSERS,
        r#"[{"session_id":"s-end","browser_instance_id":"aaa"}]"#,
    );
    let c = ctx(&d, bin, 47);
    let out = Outbox::new();
    handle(&c, 5, &out, &json!({"op": "open", "tab": 47}));
    assert!(picture(&out, Duration::from_secs(5)).is_some());

    fs::write(d.join("sessions.json"), "[]").unwrap();
    fs::remove_file(d.join("fix.png")).unwrap();
    let e = next(&out, "ended", Duration::from_secs(8)).expect("ended");
    assert_eq!(e["tab"], 47);
    assert!(wait_until(|| !streaming("s-end"), Duration::from_secs(3)));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn keep_without_a_view_is_false() {
    let d = tmp("keep");
    let c = ctx(&d, d.join("no-bsk"), 48);
    let out: Arc<Outbox> = Outbox::new();
    handle(&c, 6, &out, &json!({"op": "keep", "id": 1, "tab": 48}));
    assert_eq!(
        next(&out, "keep", Duration::from_secs(2)).unwrap()["ok"],
        false
    );
    let _ = fs::remove_dir_all(&d);
}
