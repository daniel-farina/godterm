use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::hit::{open_policy, parse_hit};
use super::{find_chrome, Ctx};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::embedded;
use crate::embedded_tests::site;
use crate::web::tap::{Out, Outbox};

#[test]
fn only_http_and_https_links_open() {
    assert_eq!(
        open_policy("https://example.com/a?b=1#c").unwrap(),
        "https://example.com/a?b=1#c"
    );
    assert_eq!(
        open_policy("HTTP://localhost:3000/x").unwrap(),
        "http://localhost:3000/x"
    );
    for bad in [
        "mailto:someone@example.com",
        "javascript:alert(1)",
        "data:text/html,hi",
        "file:///etc/hosts",
        "tel:+15550100",
        "view-source:https://example.com",
        "about:blank",
        "chrome://settings",
        "",
        "/relative/path",
    ] {
        assert!(open_policy(bad).is_err(), "{bad}");
    }
    let why = open_policy("mailto:someone@example.com").unwrap_err();
    assert!(
        why.starts_with("mailto:") && why.contains("not opened"),
        "{why}"
    );
    assert!(open_policy("https://").is_err());
    assert!(open_policy("https://exa mple.com").is_err());
}

#[test]
fn an_empty_or_missing_result_is_a_plain_page() {
    for raw in [json!(null), json!({"empty": true}), json!({})] {
        let v = parse_hit(&raw);
        assert!(v["link"].is_null() && v["image"].is_null(), "{v}");
        assert_eq!(v["selection"], "");
        assert_eq!(v["editable"], json!(false));
    }
}

#[test]
fn a_link_and_an_image_are_described() {
    let v = parse_hit(&json!({
        "link": {"href": "https://example.com/docs", "text": "Read\u{7}the docs"},
        "image": {"src": "https://example.com/a.png", "alt": "logo"},
        "selection": "chosen words",
    }));
    assert_eq!(v["link"]["href"], "https://example.com/docs");
    assert_eq!(v["link"]["text"], "Readthe docs");
    assert_eq!(v["link"]["open"], json!(true));
    assert_eq!(v["image"]["src"], "https://example.com/a.png");
    assert_eq!(v["image"]["open"], json!(true));
    assert_eq!(v["selection"], "chosen words");
}

#[test]
fn other_schemes_are_shown_but_never_openable() {
    let v = parse_hit(&json!({"link": {"href": "mailto:a@example.com", "text": "Mail"}}));
    assert_eq!(v["link"]["href"], "mailto:a@example.com");
    assert_eq!(v["link"]["open"], json!(false));
    assert!(
        v["link"]["why"]
            .as_str()
            .unwrap()
            .contains("http and https"),
        "{v}"
    );
    let v = parse_hit(&json!({"link": {"href": "javascript:void(0)", "text": ""}}));
    assert_eq!(v["link"]["open"], json!(false));
    let v = parse_hit(&json!({"image": {"src": "data:image/png;base64,AAAA", "alt": ""}}));
    assert_eq!(v["image"]["open"], json!(false));
    assert_eq!(v["image"]["src"], "");
    let long = format!("https://example.com/{}", "a".repeat(9000));
    let v = parse_hit(&json!({"link": {"href": long, "text": "x"}}));
    assert_eq!(v["link"]["open"], json!(false));
    assert_eq!(v["link"]["href"], "");
}

#[test]
fn fields_report_editable_readonly_and_hide_passwords() {
    let v = parse_hit(&json!({"editable": true, "readonly": true, "selection": "abc"}));
    assert_eq!(v["editable"], json!(true));
    assert_eq!(v["readonly"], json!(true));
    assert_eq!(v["selection"], "abc");
    let v = parse_hit(&json!({"editable": true, "password": true, "selection": "hunter2"}));
    assert_eq!(v["selection"], "");
    assert_eq!(v["password"], json!(true));
    let v = parse_hit(&json!({"editable": false, "readonly": true}));
    assert_eq!(v["readonly"], json!(false));
}

#[test]
fn a_huge_selection_is_cut() {
    let v = parse_hit(&json!({"selection": "x".repeat(50_000)}));
    assert_eq!(v["selection"].as_str().unwrap().chars().count(), 5000);
}

fn web_ask(ctx: &Ctx, out: &Arc<Outbox>, id: &str, mut m: Value) -> Value {
    m["t"] = json!("ebv");
    m["id"] = json!(id);
    m["tab"] = json!(ctx.uid);
    embedded::handle_web(ctx, 9, out, &m);
    let end = Instant::now() + Duration::from_secs(12);
    while Instant::now() < end {
        if let Some(Out::Text(t)) = out.pop(Duration::from_millis(200)) {
            let r: Value = serde_json::from_str(&t).unwrap();
            if r["id"] == json!(id) {
                return r;
            }
        }
    }
    panic!("no answer to {id}");
}

#[test]
fn a_real_browser_says_what_is_under_the_pointer() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if find_chrome("").is_none() {
        eprintln!("no Chrome here: skipped");
        return;
    }
    let (port, stop) = site();
    let ctx = Ctx {
        uid: 987_004,
        bin: String::new(),
    };
    let base = format!("http://127.0.0.1:{port}");
    let _chrome = crate::embedded_tests::OneChrome::new();
    let run = |a: Value| embedded::run(&ctx, &a);
    let v = run(
        json!({"action": "navigate", "url": format!("{base}/menu"), "width": 800, "height": 600}),
    );
    assert_eq!(v["title"], "Menu page", "{v}");
    let out = Outbox::new();
    let r = web_ask(&ctx, &out, "o", json!({"op": "open", "w": 800, "h": 600}));
    assert_eq!(r["ok"], json!(true), "{r}");
    let at = |x: f64, y: f64| {
        web_ask(
            &ctx,
            &out,
            "h",
            json!({"op": "hit", "fx": x / 800.0, "fy": y / 600.0}),
        )
    };

    let r = at(60.0, 35.0);
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["link"]["href"], format!("{base}/two"), "{r}");
    assert_eq!(r["link"]["text"], "Link text", "{r}");
    assert_eq!(r["link"]["open"], json!(true));
    assert!(r["image"].is_null(), "{r}");
    assert_eq!(r["url"], format!("{base}/menu"));
    assert_eq!(r["title"], "Menu page");

    let r = at(60.0, 75.0);
    assert_eq!(r["link"]["href"], "mailto:x@example.com", "{r}");
    assert_eq!(r["link"]["open"], json!(false), "{r}");

    let r = at(130.0, 130.0);
    assert_eq!(r["image"]["src"], format!("{base}/pic.gif"), "{r}");
    assert_eq!(r["image"]["alt"], "remote");
    let r = at(50.0, 130.0);
    assert_eq!(r["image"]["open"], json!(false), "{r}");
    assert_eq!(r["image"]["src"], "", "{r}");

    let r = at(400.0, 400.0);
    assert!(r["link"].is_null() && r["image"].is_null(), "{r}");
    assert_eq!(r["editable"], json!(false));

    run(
        json!({"action": "eval", "expression": "(()=>{const p=document.getElementById('tx');const g=getSelection();g.selectAllChildren(p);return g.toString()})()"}),
    );
    let r = at(80.0, 192.0);
    assert_eq!(r["selection"], "pick these words", "{r}");
    let r = at(500.0, 500.0);
    assert_eq!(
        r["selection"], "",
        "selection counts only under the pointer: {r}"
    );

    let r = at(100.0, 240.0);
    assert_eq!(r["editable"], json!(true), "{r}");
    assert_eq!(r["readonly"], json!(false));
    run(
        json!({"action": "eval", "expression": "(()=>{const i=document.getElementById('in');i.focus();i.setSelectionRange(0,5);return 1})()"}),
    );
    let r = at(100.0, 240.0);
    assert_eq!(r["selection"], "field", "{r}");
    run(
        json!({"action": "eval", "expression": "(()=>{const i=document.getElementById('pw');i.focus();i.select();return 1})()"}),
    );
    let r = at(100.0, 280.0);
    assert_eq!(r["editable"], json!(true));
    assert_eq!(r["password"], json!(true), "{r}");
    assert_eq!(r["selection"], "", "{r}");

    let r = at(60.0, 325.0);
    assert_eq!(
        r["link"]["href"],
        format!("{base}/three"),
        "a link in a shadow root: {r}"
    );
    assert_eq!(r["link"]["text"], "Shadow link", "{r}");
    let r = at(60.0, 370.0);
    assert_eq!(
        r["link"]["href"],
        format!("{base}/three"),
        "a link in an iframe: {r}"
    );
    assert_eq!(r["link"]["text"], "In frame", "{r}");

    let field = |what: &str, text: Option<&str>| {
        let mut m = json!({"op": "edit", "fx": 100.0 / 800.0, "fy": 240.0 / 600.0, "what": what});
        if let Some(t) = text {
            m["text"] = json!(t);
        }
        web_ask(&ctx, &out, "e", m)
    };
    let val = || {
        run(json!({"action": "eval", "expression": "document.getElementById('in').value"}))
            ["result"]
            .clone()
    };
    assert_eq!(field("selectall", None)["ok"], json!(true));
    assert_eq!(field("paste", Some("pasted"))["ok"], json!(true));
    assert_eq!(val(), json!("pasted"));
    field("selectall", None);
    field("delete", None);
    assert_eq!(val(), json!(""));
    let r = web_ask(
        &ctx,
        &out,
        "e2",
        json!({"op": "edit", "fx": 0.6, "fy": 0.9, "what": "selectall"}),
    );
    assert_eq!(r["ok"], json!(false), "{r}");
    let r = web_ask(
        &ctx,
        &out,
        "e3",
        json!({"op": "edit", "fx": 0.1, "fy": 0.4, "what": "format-disk"}),
    );
    assert_eq!(r["ok"], json!(false), "{r}");

    let src = web_ask(&ctx, &out, "s", json!({"op": "source"}));
    assert_eq!(src["ok"], json!(true), "{src}");
    assert!(src["html"].as_str().unwrap().contains("Menu page"), "{src}");

    embedded::drop_client(9);
    embedded::reap(&[]);
    assert!(embedded::live_uids().is_empty());
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn dragging_with_the_mouse_selects_text_in_a_real_browser() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if find_chrome("").is_none() {
        eprintln!("no Chrome here: skipped");
        return;
    }
    let (port, stop) = site();
    let ctx = Ctx {
        uid: 987_005,
        bin: String::new(),
    };
    let base = format!("http://127.0.0.1:{port}");
    let _chrome = crate::embedded_tests::OneChrome::new();
    let run = |a: Value| embedded::run(&ctx, &a);
    let v = run(
        json!({"action": "navigate", "url": format!("{base}/menu"), "width": 800, "height": 600}),
    );
    assert_eq!(v["title"], "Menu page", "{v}");
    let v = run(json!({"action": "eval", "expression":
        "(()=>{document.body.insertAdjacentHTML('beforeend','<p id=\"drag\" style=\"position:fixed;left:20px;top:300px;margin:0;font:20px monospace\">alpha beta gamma delta</p>');const q=document.getElementById('drag').getBoundingClientRect();return [q.left,q.top+q.height/2,q.right]})()"}));
    let r = v["result"].as_array().cloned().unwrap_or_default();
    assert_eq!(r.len(), 3, "{v}");
    let (x0, y, x1) = (
        r[0].as_f64().unwrap() + 1.0,
        r[1].as_f64().unwrap(),
        r[2].as_f64().unwrap() - 1.0,
    );
    let out = Outbox::new();
    let o = web_ask(&ctx, &out, "o", json!({"op": "open", "w": 800, "h": 600}));
    assert_eq!(o["ok"], json!(true), "{o}");
    let at = |kind: &str, x: f64, button: &str, cc: i64| {
        web_ask(
            &ctx,
            &out,
            "m",
            json!({"op": "mouse", "kind": kind, "fx": x / 800.0, "fy": y / 600.0, "button": button, "cc": cc}),
        )
    };
    at("move", x0, "none", 0);
    at("down", x0, "left", 1);
    at("move", (x0 + x1) / 2.0, "left", 0);
    at("move", x1, "left", 0);
    at("up", x1, "left", 1);
    let v = run(json!({"action": "eval", "expression": "window.getSelection().toString()"}));
    let sel = v["result"].as_str().unwrap_or("").to_string();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(
        sel.contains("alpha") && sel.contains("delta"),
        "dragging selected {sel:?}"
    );
}
