use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config::testing::LOCK as ENV_LOCK;
use crate::embedded::{self, check_url, key_events, parse_combo, Ctx};
use crate::jobs_tests::{running, setup};
use crate::web::tap::{Out, Outbox};

#[test]
fn addresses_are_http_and_https_only() {
    assert_eq!(
        check_url("localhost:3000").unwrap(),
        "http://localhost:3000"
    );
    assert_eq!(
        check_url("127.0.0.1:8080/x?y=1").unwrap(),
        "http://127.0.0.1:8080/x?y=1"
    );
    assert_eq!(check_url("example.com/a").unwrap(), "https://example.com/a");
    assert_eq!(
        check_url("HTTP://Example.com").unwrap(),
        "http://Example.com"
    );
    assert_eq!(
        check_url("http://[::1]:5173/").unwrap(),
        "http://[::1]:5173/"
    );
    assert_eq!(check_url("about:blank").unwrap(), "about:blank");
    assert_eq!(
        check_url("dev.localhost:4000").unwrap(),
        "http://dev.localhost:4000"
    );
    for bad in [
        "file:///etc/passwd",
        "FILE:///Users/x/index.html",
        "chrome://settings",
        "chrome-extension://abc/page.html",
        "javascript:alert(1)",
        "data:text/html,<b>x</b>",
        "view-source:http://localhost",
        "about:config",
        "ftp://example.com",
        "devtools://devtools/bundled/inspector.html",
        "",
        "   ",
        "http://",
        "http:///path",
        "two words",
        "http://a\nb.com",
    ] {
        assert!(check_url(bad).is_err(), "{bad:?} must be refused");
    }
    let long = format!("http://localhost/{}", "a".repeat(3000));
    assert!(check_url(&long).is_err());
    let e = check_url("file:///tmp/x.html").unwrap_err();
    assert!(e.contains("serve the folder"), "{e}");
}

#[test]
fn local_hosts_are_told_apart() {
    for h in [
        "localhost",
        "app.localhost",
        "127.0.0.1",
        "127.1.2.3",
        "::1",
        "[::1]",
        "192.168.1.5",
        "0.0.0.0",
    ] {
        assert!(embedded::is_local_host(h), "{h}");
    }
    for h in ["example.com", "localhost.evil.com", "10.0.0.1x"] {
        assert!(!embedded::is_local_host(h), "{h}");
    }
}

#[test]
fn key_combos_become_chrome_key_events() {
    assert_eq!(parse_combo("Enter"), ("Enter".to_string(), 0));
    assert_eq!(parse_combo("Control+A"), ("A".to_string(), 2));
    assert_eq!(parse_combo("ctrl+shift+z"), ("z".to_string(), 10));
    assert_eq!(parse_combo("Cmd+v"), ("v".to_string(), 4));
    assert_eq!(parse_combo("esc"), ("Escape".to_string(), 0));
    assert_eq!(parse_combo("down"), ("ArrowDown".to_string(), 0));
    assert_eq!(parse_combo("+"), ("+".to_string(), 0));
    assert_eq!(parse_combo("Shift"), ("Shift".to_string(), 0));
    let ev = key_events("a", "KeyA", 0, None);
    assert_eq!(ev[0]["type"], "keyDown");
    assert_eq!(ev[0]["text"], "a");
    assert_eq!(ev[0]["windowsVirtualKeyCode"], 65);
    assert_eq!(ev[1]["type"], "keyUp");
    let ev = key_events("Enter", "Enter", 0, None);
    assert_eq!(ev[0]["text"], "\r");
    let ev = key_events("ArrowLeft", "ArrowLeft", 0, None);
    assert_eq!(ev[0]["type"], "rawKeyDown");
    assert_eq!(ev[0]["windowsVirtualKeyCode"], 37);
    assert!(ev[0].get("text").is_none());
    let ev = key_events("a", "KeyA", 2, None);
    assert_eq!(ev[0]["type"], "rawKeyDown");
    assert_eq!(ev[0]["commands"], json!(["selectAll"]));
    let ev = key_events("A", "KeyA", 8, None);
    assert_eq!(ev[0]["type"], "keyDown", "shift keeps text");
    let ev = key_events("F5", "F5", 0, None);
    assert_eq!(ev[0]["windowsVirtualKeyCode"], 116);
}

#[test]
fn the_tool_is_for_the_tab_itself_and_off_by_default() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("embedded-tool");
    let a = running(&mut app, 0, "site", "sess-e");
    let uid = app.panes[0].tabs[a].uid;
    assert!(crate::tab_jobs::TAB_TOOLS.contains(&"embedded_browser"));
    assert!(crate::tab_jobs::tab_tool_list()
        .iter()
        .any(|t| t["name"] == "embedded_browser"
            && t["inputSchema"]["properties"]["action"]["enum"]
                .as_array()
                .is_some_and(|e| e.contains(&json!("screenshot")))
            && !t["inputSchema"]["properties"]
                .as_object()
                .unwrap()
                .contains_key("tab")));
    let desc = crate::tab_jobs::tab_description("embedded_browser");
    assert!(
        desc.contains("BrowserSkill") && desc.contains("localhost"),
        "{desc}"
    );

    let v = app.tab_call(uid, "embedded_browser", &json!({"action": "status"}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(v["error"].as_str().unwrap().contains("off"), "{v}");

    app.cfg.web.embedded_browser = true;
    let v = app.tab_call(uid, "embedded_browser", &json!({"action": "status"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["_embedded"]["uid"], json!(uid));

    let other = crate::control::tab_id(uid + 1);
    let v = app.tab_call(
        uid,
        "embedded_browser",
        &json!({"action": "status", "tab": other}),
    );
    assert!(v["error"].as_str().unwrap().contains("own tab"), "{v}");
    let v = app.tab_call(
        uid,
        "embedded_browser",
        &json!({"action": "status", "account": 2}),
    );
    assert!(v["error"].as_str().unwrap().contains("takes no"), "{v}");
    let v = app.tab_call(999_999, "embedded_browser", &json!({"action": "status"}));
    assert_eq!(v["error"], json!("this tab is gone"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_screenshot_reaches_the_agent_as_an_image() {
    let mut call = |_: &str, _: &Value| {
        Ok(
            json!({"ok": true, "path": "/tmp/x.jpg", "image_b64": "AAAA", "image_type": "image/jpeg"}),
        )
    };
    let r = crate::mcp::handle_as(
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "embedded_browser", "arguments": {}}}),
        &mut call,
        false,
    )
    .unwrap();
    let c = r["result"]["content"].as_array().unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[1]["type"], "image");
    assert_eq!(c[1]["data"], "AAAA");
    assert_eq!(c[1]["mimeType"], "image/jpeg");
    let text = c[0]["text"].as_str().unwrap();
    assert!(
        !text.contains("AAAA") && text.contains("/tmp/x.jpg"),
        "{text}"
    );
}

#[test]
fn nothing_runs_without_a_tab_or_chrome() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _chrome = OneChrome::new();
    let ctx = Ctx {
        uid: 987_001,
        bin: "/nonexistent/chrome".into(),
    };
    let v = embedded::run(&ctx, &json!({"action": "status"}));
    assert_eq!(v["open"], json!(false));
    assert_eq!(v["owned_pages"], json!(0));
    let v = embedded::run(&ctx, &json!({"action": "click", "ref": 1}));
    assert_eq!(v["ok"], json!(false));
    assert!(v["error"].as_str().unwrap().contains("navigate"), "{v}");
    let v = embedded::run(
        &ctx,
        &json!({"action": "navigate", "url": "file:///etc/passwd"}),
    );
    assert!(
        v["error"].as_str().unwrap().contains("serve the folder"),
        "{v}"
    );
    let v = embedded::run(
        &ctx,
        &json!({"action": "navigate", "url": "http://localhost:1"}),
    );
    assert_eq!(v["ok"], json!(false));
    assert!(embedded::live_uids().is_empty());
    assert_eq!(embedded::browser_pid(), None, "no browser was started");
    let v = embedded::run(&ctx, &json!({"action": "close"}));
    assert_eq!(v["closed"], json!(false));
}

pub(crate) fn site() -> (u16, Arc<std::sync::atomic::AtomicBool>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s2 = Arc::clone(&stop);
    l.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        while !s2.load(std::sync::atomic::Ordering::Relaxed) {
            let Ok((mut c, _)) = l.accept() else {
                std::thread::sleep(Duration::from_millis(15));
                continue;
            };
            std::thread::spawn(move || {
                c.set_nonblocking(false).ok();
                c.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let mut buf = [0u8; 2048];
                let n = c.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let Some(path) = req.split_whitespace().nth(1).map(str::to_string) else {
                    return;
                };
                let (status, body) = match path.as_str() {
                    "/" => (
                        "200 OK",
                        r#"<html><head><title>Home page</title></head><body><h1 id="h">Welcome home</h1><a id="l" href="/two" style="position:absolute;left:20px;top:80px;display:block;width:120px;height:30px">Go to two</a><input id="i" style="position:absolute;left:20px;top:140px;width:200px"><button id="b" style="position:absolute;left:20px;top:200px;width:100px;height:40px" onclick="document.getElementById('h').textContent='Clicked ' + document.getElementById('i').value;console.log('button pressed')">Press</button><script>console.error('startup boom');fetch('/missing.js')</script></body></html>"#,
                    ),
                    "/two" => (
                        "200 OK",
                        r#"<html><head><title>Second</title></head><body><p>This is page two</p><div style="height:3000px"></div><p id="end">The end</p></body></html>"#,
                    ),
                    "/pop" => (
                        "200 OK",
                        r#"<html><head><title>Pop</title></head><body><a id="blank" target="_blank" href="/two" style="position:absolute;left:20px;top:20px;display:block;width:160px;height:30px">Open in a new tab</a><script>console.log('pop page')</script></body></html>"#,
                    ),
                    "/menu" => (
                        "200 OK",
                        r#"<html><head><title>Menu page</title></head><body style="margin:0"><a id="ln" href="/two" style="position:absolute;left:20px;top:20px;width:160px;height:30px;display:block">Link text</a><a id="mail" href="mailto:x@example.com" style="position:absolute;left:20px;top:60px;width:160px;height:30px;display:block">Mail me</a><img id="im" alt="pic" src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" style="position:absolute;left:20px;top:100px;width:60px;height:60px"><img id="im2" alt="remote" src="/pic.gif" style="position:absolute;left:100px;top:100px;width:60px;height:60px"><p id="tx" style="position:absolute;left:20px;top:180px;margin:0;font-size:20px">pick these words</p><input id="in" value="field text" style="position:absolute;left:20px;top:230px;width:200px"><input id="pw" type="password" value="secret" style="position:absolute;left:20px;top:270px;width:200px"><div id="host" style="position:absolute;left:20px;top:310px;width:200px;height:30px"></div><iframe id="fr" srcdoc="<a href='/three' style='display:block;width:100px;height:40px'>In frame</a>" style="position:absolute;left:20px;top:350px;width:200px;height:60px;border:0"></iframe><script>const r=document.getElementById('host').attachShadow({mode:'open'});r.innerHTML='<a href="/three" style="display:block;width:200px;height:30px">Shadow link</a>'</script></body></html>"#,
                    ),
                    "/three" => (
                        "200 OK",
                        r#"<html><head><title>Third</title></head><body><p>Third page</p><script>console.log('third page')</script></body></html>"#,
                    ),
                    _ => ("404 Not Found", "nope"),
                };
                let _ = write!(
                c,
                "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            });
        }
    });
    (port, stop)
}

pub(crate) struct OneChrome;

fn fresh() {
    embedded::shutdown_all();
    embedded::set_page_cap(embedded::MAX_TOTAL);
    embedded::set_sleep_after(Duration::from_secs(embedded::SLEEP_MINUTES * 60));
    embedded::set_linger(Duration::from_secs(180));
}

impl OneChrome {
    pub(crate) fn new() -> OneChrome {
        fresh();
        OneChrome
    }
}

impl Drop for OneChrome {
    fn drop(&mut self) {
        fresh();
    }
}

pub(crate) fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn a_real_browser_is_driven_and_cleaned_up() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if embedded::find_chrome("").is_none() {
        eprintln!("no Chrome here: skipped");
        return;
    }
    let (port, stop) = site();
    let ctx = Ctx {
        uid: 987_002,
        bin: String::new(),
    };
    let base = format!("http://127.0.0.1:{port}");
    let _chrome = OneChrome::new();
    let run = |a: Value| embedded::run(&ctx, &a);

    let v =
        run(json!({"action": "navigate", "url": format!("{base}/"), "width": 800, "height": 600}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["title"], "Home page", "{v}");
    assert_eq!(v["loaded"], json!(true));
    assert_eq!(embedded::live_uids(), vec![987_002]);

    let dir = embedded::root_dir();
    let mine: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("p{}-b", std::process::id()))
        })
        .collect();
    assert_eq!(mine.len(), 1, "one temp profile for the one browser");
    let owner = std::fs::read_to_string(mine[0].path().join("owner")).unwrap();
    let chrome_pid: u32 = owner.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(alive(chrome_pid));
    assert_eq!(embedded::browser_pid(), Some(chrome_pid));
    let args = String::from_utf8_lossy(
        &std::process::Command::new("ps")
            .args(["-p", &chrome_pid.to_string(), "-o", "command="])
            .output()
            .unwrap()
            .stdout,
    )
    .to_string();
    assert!(
        args.contains("--remote-debugging-port=0")
            && args.contains("--remote-debugging-address=127.0.0.1")
            && args.contains("--no-startup-window"),
        "{args}"
    );
    assert!(
        args.contains(&*mine[0].path().to_string_lossy()),
        "its own user data dir: {args}"
    );
    assert!(
        !args.contains("/Library/Application Support/Google/Chrome/Default"),
        "{args}"
    );

    let v = run(json!({"action": "text"}));
    assert!(v["text"].as_str().unwrap().contains("Welcome home"), "{v}");
    let v = run(json!({"action": "text", "selector": "#nope"}));
    assert_eq!(v["ok"], json!(false), "{v}");

    let v = run(json!({"action": "snapshot"}));
    let els = v["elements"].as_array().unwrap();
    let link = els
        .iter()
        .find(|e| e["label"] == "Go to two")
        .unwrap_or_else(|| panic!("{v}"));
    assert_eq!(link["tag"], "a");
    let input = els.iter().find(|e| e["tag"] == "input").unwrap();
    let button = els.iter().find(|e| e["label"] == "Press").unwrap();

    let v = run(json!({"action": "type", "ref": input["ref"], "text": "hello"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let v = run(json!({"action": "click", "ref": button["ref"]}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let v = run(json!({"action": "text", "selector": "#h"}));
    assert_eq!(v["text"], "Clicked hello", "{v}");

    let v = run(json!({"action": "console"}));
    let texts: Vec<String> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            format!(
                "{}:{}",
                e["kind"].as_str().unwrap(),
                e["text"].as_str().unwrap()
            )
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("error:startup boom")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("log:button pressed")),
        "{texts:?}"
    );
    let v = run(json!({"action": "console", "level": "error"}));
    assert!(
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["kind"] != "log"),
        "{v}"
    );
    let cursor = v["cursor"].as_u64().unwrap();
    let v = run(json!({"action": "console", "since": cursor}));
    assert!(v["entries"].as_array().unwrap().is_empty(), "{v}");

    let v = run(json!({"action": "network"}));
    let urls: Vec<String> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["url"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(urls.iter().any(|u| u.ends_with("/missing.js")), "{urls:?}");
    assert!(v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(
            |e| e["status"].as_i64().unwrap_or(-1) >= 400 || e["status"].as_i64().unwrap_or(-1) < 0
        ));
    let v = run(json!({"action": "network", "all": true}));
    assert!(
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["status"] == 200),
        "{v}"
    );

    let v = run(json!({"action": "screenshot"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let path = std::path::PathBuf::from(v["path"].as_str().unwrap());
    let file = std::fs::read(&path).unwrap();
    assert!(file.starts_with(&[0xff, 0xd8]), "a jpeg");
    assert_eq!(v["size"]["width"], json!(800), "{v}");
    assert!(v["image_b64"].as_str().unwrap().len() > 100);
    assert_eq!(v["image_type"], "image/jpeg");

    let v = run(json!({"action": "click", "text": "Go to two"}));
    assert_eq!(v["navigated"], json!(true), "{v}");
    assert_eq!(v["title"], "Second", "{v}");
    assert!(v["url"].as_str().unwrap().ends_with("/two"), "{v}");
    let v = run(json!({"action": "scroll", "selector": "#end"}));
    assert!(v["scroll"]["y"].as_i64().unwrap() > 1000, "{v}");
    let v = run(json!({"action": "scroll", "dy": -100000}));
    assert_eq!(v["scroll"]["y"], json!(0), "{v}");
    let v = run(json!({"action": "screenshot", "full_page": true, "format": "png"}));
    assert!(v["size"]["height"].as_i64().unwrap() > 3000, "{v}");
    assert!(std::fs::read(v["path"].as_str().unwrap())
        .unwrap()
        .starts_with(b"\x89PNG"));
    let v = run(json!({"action": "back"}));
    assert_eq!(v["title"], "Home page", "{v}");
    let v = run(json!({"action": "forward"}));
    assert_eq!(v["title"], "Second", "{v}");
    let v = run(json!({"action": "eval", "expression": "document.title + '!'"}));
    assert_eq!(v["result"], "Second!", "{v}");
    let v = run(json!({"action": "eval", "expression": "throw new Error('bad')"}));
    assert_eq!(v["ok"], json!(false), "{v}");
    let v = run(json!({"action": "wait", "text": "The end"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let v = run(json!({"action": "wait", "selector": "#never", "timeout_ms": 300}));
    assert_eq!(v["ok"], json!(false), "{v}");
    let v = run(json!({"action": "navigate", "url": "http://127.0.0.1:1/"}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(v["error"].as_str().unwrap().contains("did not load"), "{v}");
    let v = run(json!({"action": "navigate", "url": "chrome://settings"}));
    assert!(v["error"].as_str().unwrap().contains("not opened"), "{v}");
    let v = run(json!({"action": "navigate", "url": "file:///etc/hosts"}));
    assert!(
        v["error"].as_str().unwrap().contains("serve the folder"),
        "{v}"
    );
    let v = run(json!({"action": "navigate", "url": format!("{base}/two")}));
    assert_eq!(v["title"], "Second", "{v}");
    let v = run(json!({"action": "resize", "device": "phone"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let v = run(json!({"action": "eval", "expression": "innerWidth"}));
    assert_eq!(v["result"], json!(390), "{v}");
    let v = run(json!({"action": "status"}));
    assert_eq!(v["viewport"]["width"], json!(390), "{v}");
    assert_eq!(v["owned_pages"], json!(1), "{v}");
    assert_eq!(v["asleep_pages"], json!(0), "{v}");
    assert!(v.get("woke").is_none() && v.get("asleep").is_none(), "{v}");

    let out = Outbox::new();
    embedded::handle_web(
        &ctx,
        7,
        &out,
        &json!({"op": "open", "id": "e1", "tab": 987002, "w": 800, "h": 600}),
    );
    let mut opened = None;
    let mut pic = None;
    let end = Instant::now() + Duration::from_secs(6);
    while (opened.is_none() || pic.is_none()) && Instant::now() < end {
        match out.pop(Duration::from_millis(200)) {
            Some(Out::Text(t)) => {
                let r: Value = serde_json::from_str(&t).unwrap();
                if r["op"] == "open" {
                    opened = Some(r);
                }
            }
            Some(Out::Picture(b)) => pic = Some(b),
            _ => {}
        }
    }
    let r = opened.expect("open answered");
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(
        (r["w"].as_u64(), r["h"].as_u64()),
        (Some(800), Some(600)),
        "{r}"
    );
    let b = pic.expect("a picture");
    let (kind, uid, jpg) = crate::web::ws::unframe(&b).unwrap();
    assert_eq!((kind, uid), (embedded::KIND_FRAME, 987_002));
    assert!(jpg.starts_with(&[0xff, 0xd8]));

    embedded::handle_web(
        &ctx,
        7,
        &out,
        &json!({"op": "go", "id": "e2", "tab": 987002, "url": format!("{base}/")}),
    );
    let mut went = None;
    let end = Instant::now() + Duration::from_secs(8);
    while went.is_none() && Instant::now() < end {
        if let Some(Out::Text(t)) = out.pop(Duration::from_millis(200)) {
            let r: Value = serde_json::from_str(&t).unwrap();
            if r["op"] == "go" {
                went = Some(r);
            }
        }
    }
    assert_eq!(went.unwrap()["title"], "Home page");
    let v = run(json!({"action": "snapshot"}));
    let input = v["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["tag"] == "input")
        .unwrap()
        .clone();
    let fx = input["x"].as_f64().unwrap() / 800.0;
    let fy = input["y"].as_f64().unwrap() / 600.0;
    for kind in ["move", "down", "up"] {
        embedded::handle_web(
            &ctx,
            7,
            &out,
            &json!({"op": "mouse", "id": format!("m{kind}"), "tab": 987002, "kind": kind, "fx": fx, "fy": fy, "button": "left", "cc": 1}),
        );
    }
    embedded::handle_web(
        &ctx,
        7,
        &out,
        &json!({"op": "key", "id": "k1", "tab": 987002, "key": "h", "code": "KeyH", "mods": 0}),
    );
    embedded::handle_web(
        &ctx,
        7,
        &out,
        &json!({"op": "text", "id": "k2", "tab": 987002, "text": "ey"}),
    );
    let v = run(json!({"action": "eval", "expression": "document.getElementById('i').value"}));
    assert_eq!(v["result"], "hey", "typed through the pane: {v}");
    embedded::handle_web(
        &ctx,
        7,
        &out,
        &json!({"op": "mouse", "id": "w1", "tab": 987002, "kind": "wheel", "fx": 0.5, "fy": 0.5, "dx": 0, "dy": 0}),
    );
    embedded::handle_web(
        &ctx,
        7,
        &out,
        &json!({"op": "go", "id": "e3", "tab": 987002, "url": "file:///etc/hosts"}),
    );
    let mut refused = None;
    let end = Instant::now() + Duration::from_secs(4);
    while refused.is_none() && Instant::now() < end {
        if let Some(Out::Text(t)) = out.pop(Duration::from_millis(200)) {
            let r: Value = serde_json::from_str(&t).unwrap();
            if r["id"] == "e3" {
                refused = Some(r);
            }
        }
    }
    assert_eq!(refused.unwrap()["ok"], json!(false));

    embedded::drop_client(7);
    embedded::reap(&[987_002]);
    assert_eq!(
        embedded::live_uids(),
        vec![987_002],
        "a tab that is open keeps its pages"
    );
    embedded::reap(&[]);
    assert!(
        embedded::live_uids().is_empty(),
        "a closed tab takes its pages along"
    );
    assert_eq!(embedded::live_pages(), 0);
    embedded::housekeep();
    assert!(
        alive(chrome_pid) && embedded::browser_pid() == Some(chrome_pid),
        "the browser lingers for a while after its last page"
    );
    embedded::set_linger(Duration::ZERO);
    embedded::housekeep();
    assert_eq!(embedded::browser_pid(), None);
    let end = Instant::now() + Duration::from_secs(5);
    while (alive(chrome_pid) || mine[0].path().exists()) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!alive(chrome_pid), "chrome is gone");
    assert!(!mine[0].path().exists(), "its profile and cookies are gone");
    let v = run(json!({"action": "close"}));
    assert_eq!(v["closed"], json!(false));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn tab_ids(v: &Value) -> Vec<u64> {
    v["tabs"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|t| t["id"].as_u64().unwrap())
        .collect()
}

pub(crate) fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_real_browser_keeps_page_tabs() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if embedded::find_chrome("").is_none() {
        eprintln!("no Chrome here: skipped");
        return;
    }
    let (port, stop) = site();
    let ctx = Ctx {
        uid: 987_003,
        bin: String::new(),
    };
    let base = format!("http://127.0.0.1:{port}");
    let _chrome = OneChrome::new();
    let run = |a: Value| embedded::run(&ctx, &a);

    let v = run(json!({"action": "tabs"}));
    assert_eq!(v["open"], json!(false), "{v}");
    let v = run(
        json!({"action": "navigate", "url": format!("{base}/pop"), "width": 800, "height": 600}),
    );
    assert_eq!(v["title"], "Pop", "{v}");
    let v = run(json!({"action": "tabs"}));
    assert_eq!(tab_ids(&v).len(), 1, "{v}");
    let one = tab_ids(&v)[0];
    assert_eq!(v["active"], json!(one));

    let v = run(json!({"action": "click", "selector": "#blank"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    wait_for("the target=_blank tab", || {
        tab_ids(&run(json!({"action": "tabs"}))).len() == 2
    });
    wait_for("the new tab to load", || {
        run(json!({"action": "status"}))["title"] == "Second"
    });
    let v = run(json!({"action": "tabs"}));
    let two = tab_ids(&v)[1];
    assert!(two > one, "{v}");
    assert_eq!(
        v["active"],
        json!(two),
        "a new window becomes the active tab: {v}"
    );
    assert_eq!(v["tabs"][0]["title"], "Pop", "{v}");
    assert!(
        v["tabs"][1]["url"].as_str().unwrap().ends_with("/two"),
        "{v}"
    );
    let v = run(json!({"action": "text"}));
    assert!(v["text"].as_str().unwrap().contains("page two"), "{v}");

    let v = run(json!({"action": "eval", "expression": "console.log('only on two'); 1"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let v = run(json!({"action": "switch_tab", "id": one}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["title"], "Pop", "{v}");
    let v = run(json!({"action": "console"}));
    let texts = serde_json::to_string(&v["entries"]).unwrap();
    assert!(
        texts.contains("pop page") && !texts.contains("only on two"),
        "{texts}"
    );
    let v = run(json!({"action": "switch_tab", "id": two}));
    let v2 = run(json!({"action": "console"}));
    assert!(
        serde_json::to_string(&v2["entries"])
            .unwrap()
            .contains("only on two"),
        "{v} {v2}"
    );
    let v = run(json!({"action": "switch_tab", "id": 99_000_000}));
    assert_eq!(v["ok"], json!(false), "{v}");

    let v =
        run(json!({"action": "eval", "expression": format!("window.open('{base}/three') && 1")}));
    assert_eq!(v["ok"], json!(true), "{v}");
    wait_for("the window.open tab", || {
        run(json!({"action": "status"}))["title"] == "Third"
    });
    let three = tab_ids(&run(json!({"action": "tabs"})));
    assert_eq!(three.len(), 3);
    assert_eq!(three[..2], [one, two]);

    let v = run(json!({"action": "eval", "expression": "window.open('file:///etc/hosts') && 1"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    std::thread::sleep(Duration::from_millis(1500));
    let v = run(json!({"action": "tabs"}));
    assert!(
        v["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| !t["url"].as_str().unwrap().starts_with("file:")),
        "no page shows a refused scheme: {v}"
    );
    for extra in tab_ids(&run(json!({"action": "tabs"}))) {
        if !three.contains(&extra) {
            let v = run(json!({"action": "close_tab", "id": extra}));
            assert_eq!(v["ok"], json!(true), "{v}");
        }
    }
    assert_eq!(tab_ids(&run(json!({"action": "tabs"}))), three);

    let v = run(json!({"action": "new_tab", "url": format!("{base}/three")}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["title"], "Third", "{v}");
    let v = run(json!({"action": "new_tab"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let v = run(json!({"action": "new_tab"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let v = run(json!({"action": "tabs"}));
    assert_eq!(tab_ids(&v).len(), 6, "{v}");
    let v = run(json!({"action": "new_tab"}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(v["error"].as_str().unwrap().contains("6 tabs"), "{v}");
    let v = run(json!({"action": "new_tab", "url": "file:///etc/hosts"}));
    assert_eq!(v["ok"], json!(false), "{v}");
    let v = run(json!({"action": "eval", "expression": "window.open('/two') ? 1 : 0"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(
        tab_ids(&run(json!({"action": "tabs"}))).len(),
        6,
        "a seventh window is closed again"
    );

    let out = Outbox::new();
    embedded::handle_web(
        &ctx,
        9,
        &out,
        &json!({"op": "open", "id": "o1", "tab": 987003, "w": 800, "h": 600}),
    );
    let mut got = None;
    let end = Instant::now() + Duration::from_secs(6);
    while got.is_none() && Instant::now() < end {
        if let Some(Out::Text(t)) = out.pop(Duration::from_millis(200)) {
            let r: Value = serde_json::from_str(&t).unwrap();
            if r["op"] == "open" {
                got = Some(r);
            }
        }
    }
    let r = got.expect("open answered");
    assert_eq!(r["tabs"].as_array().unwrap().len(), 6, "{r}");
    assert!(r["active"].as_u64().unwrap() > 0, "{r}");
    let six = tab_ids(&r);
    let ask = |op: &str, extra: Value| {
        let mut m = json!({"op": op, "id": format!("x-{op}"), "tab": 987003});
        for (k, v) in extra.as_object().unwrap() {
            m[k] = v.clone();
        }
        embedded::handle_web(&ctx, 9, &out, &m);
        let end = Instant::now() + Duration::from_secs(12);
        while Instant::now() < end {
            if let Some(Out::Text(t)) = out.pop(Duration::from_millis(200)) {
                let r: Value = serde_json::from_str(&t).unwrap();
                if r["id"] == json!(format!("x-{op}")) {
                    return r;
                }
            }
        }
        panic!("no answer to {op}");
    };
    let r = ask("tab_switch", json!({"page": one}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["active"], json!(one), "{r}");
    let r = ask("tab_close", json!({"page": six[5]}));
    assert_eq!(r["tabs"].as_array().unwrap().len(), 5, "{r}");
    let r = ask("tab_new", json!({"url": format!("{base}/")}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["tabs"].as_array().unwrap().len(), 6, "{r}");
    let r = ask("tab_new", json!({}));
    assert_eq!(r["ok"], json!(false), "{r}");
    let r = ask("tab_switch", json!({"page": 77_000_000}));
    assert_eq!(r["ok"], json!(false), "{r}");

    for _ in 0..5 {
        let v = run(json!({"action": "tabs"}));
        let first = v["tabs"][0]["id"].as_u64().unwrap();
        let v = run(json!({"action": "close_tab", "id": first}));
        assert_eq!(v["ok"], json!(true), "{v}");
    }
    let v = run(json!({"action": "tabs"}));
    assert_eq!(tab_ids(&v).len(), 1, "{v}");
    let only = tab_ids(&v)[0];
    let v = run(json!({"action": "close_tab", "id": only}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let v = run(json!({"action": "tabs"}));
    assert_eq!(
        tab_ids(&v).len(),
        1,
        "closing the last tab leaves one blank page: {v}"
    );
    assert_ne!(tab_ids(&v)[0], only);
    assert_eq!(v["tabs"][0]["url"], "about:blank", "{v}");
    let v = run(json!({"action": "navigate", "url": format!("{base}/three")}));
    assert_eq!(v["title"], "Third", "{v}");

    let v = run(json!({"action": "eval", "expression": "window.close(); 1"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    std::thread::sleep(Duration::from_millis(800));
    let v = run(json!({"action": "tabs"}));
    assert!(!tab_ids(&v).is_empty(), "never zero pages: {v}");

    embedded::drop_client(9);
    embedded::reap(&[]);
    assert!(embedded::live_uids().is_empty());
    assert_eq!(embedded::live_pages(), 0);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn the_old_browser_limit_is_read_harmlessly_and_no_longer_a_setting() {
    let cfg: crate::config::Config = toml::from_str(
        "[web]\nembedded_browser = true\nembedded_browser_max = 3\nembedded_browser_sleep_minutes = 0\n",
    )
    .unwrap();
    assert!(cfg.web.embedded_browser);
    assert_eq!(cfg.web.embedded_browser_max_pages, 40);
    assert_eq!(cfg.web.embedded_browser_sleep_minutes, 0);
    let d = crate::config::Config::default();
    assert_eq!(
        (
            d.web.embedded_browser_max_pages,
            d.web.embedded_browser_sleep_minutes
        ),
        (embedded::MAX_TOTAL, embedded::SLEEP_MINUTES)
    );
    let keys: Vec<String> = crate::settings::search(&d, "embedded browser")
        .into_iter()
        .filter_map(|(_, s)| match s.key {
            crate::settings::Key::Global(k) => Some(k.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        keys.contains(&"web.embedded_browser_max_pages".to_string()),
        "{keys:?}"
    );
    assert!(
        keys.contains(&"web.embedded_browser_sleep_minutes".to_string()),
        "{keys:?}"
    );
    assert!(
        !keys.contains(&"web.embedded_browser_max".to_string()),
        "{keys:?}"
    );
    for desc in [
        crate::tab_jobs::tab_description("embedded_browser").to_string(),
        crate::control::TOOLS
            .iter()
            .find(|t| t.name == "embedded_browser")
            .map(|t| t.description.to_string())
            .unwrap_or_default(),
    ] {
        assert!(
            desc.contains("asleep") && desc.contains("woke true"),
            "{desc}"
        );
        assert!(
            !desc.contains("number run at once") && !desc.contains("browsers at once"),
            "agents are told about pages, not about a limit on browsers: {desc}"
        );
    }
}
