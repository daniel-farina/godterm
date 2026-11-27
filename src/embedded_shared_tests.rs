use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::embedded_tests::{alive, site, tab_ids, wait_for, OneChrome};
use crate::web::tap::{Out, Outbox};

fn ctx(uid: u64) -> Ctx {
    Ctx {
        uid,
        bin: String::new(),
    }
}

fn go(uid: u64, a: Value) -> Value {
    run(&ctx(uid), &a)
}

fn ask(uid: u64, client: u64, out: &Arc<Outbox>, id: &str, mut m: Value) -> Value {
    m["t"] = json!("ebv");
    m["id"] = json!(id);
    m["tab"] = json!(uid);
    handle_web(&ctx(uid), client, out, &m);
    let end = Instant::now() + Duration::from_secs(20);
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

fn picture(out: &Arc<Outbox>) -> Option<u64> {
    let end = Instant::now() + Duration::from_secs(8);
    while Instant::now() < end {
        if let Some(Out::Picture(b)) = out.pop(Duration::from_millis(200)) {
            let (kind, uid, jpg) = crate::web::ws::unframe(&b).unwrap();
            assert_eq!(kind, KIND_FRAME);
            assert!(jpg.starts_with(&[0xff, 0xd8]));
            return Some(uid);
        }
    }
    None
}

fn profiles() -> Vec<std::path::PathBuf> {
    std::fs::read_dir(root_dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name().is_some_and(|n| {
                        n.to_string_lossy()
                            .starts_with(&format!("p{}-b", std::process::id()))
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn rss_mb(dir: &std::path::Path) -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-axo", "rss=,command="])
        .output()
        .unwrap();
    let needle = dir.to_string_lossy().to_string();
    let kb: f64 = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(&needle))
        .filter_map(|l| l.split_whitespace().next()?.parse::<f64>().ok())
        .sum();
    kb / 1024.0
}

fn page_of(uid: u64, id: u64) -> (String, String) {
    let s = get(uid).unwrap();
    let g = lk(&s.st);
    let p = g.tabs.get(id as u32).unwrap();
    (p.target.clone(), p.sid.clone())
}

fn nap(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

#[test]
fn two_tabs_share_one_browser_and_own_only_their_pages() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if find_chrome("").is_none() {
        eprintln!("no Chrome here: skipped");
        return;
    }
    let _chrome = OneChrome::new();
    let (port, stop) = site();
    let base = format!("http://127.0.0.1:{port}");
    let (a, b) = (987_101, 987_102);

    let v = go(
        a,
        json!({"action": "navigate", "url": format!("{base}/"), "width": 800, "height": 600}),
    );
    assert_eq!(v["title"], "Home page", "{v}");
    let pid = browser_pid().expect("a browser runs");
    let v = go(
        b,
        json!({"action": "navigate", "url": format!("{base}/pop"), "width": 640, "height": 480}),
    );
    assert_eq!(v["title"], "Pop", "{v}");
    assert_eq!(
        browser_pid(),
        Some(pid),
        "the second tab uses the same browser"
    );
    assert_eq!(profiles().len(), 1, "one browser, one profile");
    assert_eq!(live_uids(), vec![a, b]);
    assert_eq!(live_pages(), 2);

    let ta = tab_ids(&go(a, json!({"action": "tabs"})));
    let tb = tab_ids(&go(b, json!({"action": "tabs"})));
    assert_eq!((ta.len(), tb.len()), (1, 1));
    assert_ne!(ta[0], tb[0], "page ids are never shared");
    let v = go(a, json!({"action": "text"}));
    assert!(v["text"].as_str().unwrap().contains("Welcome home"), "{v}");
    let v = go(b, json!({"action": "status"}));
    assert_eq!(v["title"], "Pop", "{v}");
    assert_eq!(v["owned_pages"], json!(1), "{v}");
    assert_eq!(v["viewport"]["width"], json!(640), "{v}");
    let v = go(a, json!({"action": "eval", "expression": "innerWidth"}));
    assert_eq!(
        v["result"],
        json!(800),
        "each tab keeps its own viewport: {v}"
    );

    let v = go(a, json!({"action": "switch_tab", "id": tb[0]}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(v["error"].as_str().unwrap().contains("no tab"), "{v}");
    let v = go(a, json!({"action": "close_tab", "id": tb[0]}));
    assert_eq!(v["ok"], json!(false), "{v}");
    let (oa, ob) = (Outbox::new(), Outbox::new());
    let r = ask(a, 1, &oa, "oa", json!({"op": "open", "w": 800, "h": 600}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(tab_ids(&r), ta, "the pane lists only this tab's pages: {r}");
    let r = ask(b, 2, &ob, "ob", json!({"op": "open", "w": 640, "h": 480}));
    assert_eq!(tab_ids(&r), tb, "{r}");
    assert_eq!(picture(&oa), Some(a), "tab A gets its own pictures");
    assert_eq!(
        picture(&ob),
        Some(b),
        "tab B gets its own pictures at the same time"
    );
    for (op, id) in [("tab_switch", "ws"), ("tab_close", "wc")] {
        let r = ask(a, 1, &oa, id, json!({"op": op, "page": tb[0]}));
        assert_eq!(r["ok"], json!(false), "{op}: {r}");
    }
    let (target_b, sid_b) = page_of(b, tb[0]);
    let sa = get(a).unwrap();
    let chrome = browser().unwrap();
    let e = sa
        .call_on(
            &sid_b,
            "Runtime.evaluate",
            json!({"expression": "document.title = 'taken'"}),
            CALL_TIMEOUT,
        )
        .unwrap_err();
    assert_eq!(e, NOT_OWNED);
    assert_eq!(
        sa.target_op("Target.closeTarget", &target_b).unwrap_err(),
        NOT_OWNED
    );
    assert!(!chrome.close_for(a, &target_b));
    assert!(chrome
        .call_for(a, &sid_b, "Page.reload", json!({}), CALL_TIMEOUT)
        .is_err());
    nap(300);
    let v = go(b, json!({"action": "status"}));
    assert_eq!(v["title"], "Pop", "tab B's page was not touched: {v}");
    assert_eq!(tab_ids(&go(b, json!({"action": "tabs"}))), tb);

    let v = go(b, json!({"action": "click", "selector": "#blank"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    wait_for("the popup of tab B", || {
        tab_ids(&go(b, json!({"action": "tabs"}))).len() == 2
    });
    wait_for("the popup to load", || {
        go(b, json!({"action": "status"}))["title"] == "Second"
    });
    let tb2 = tab_ids(&go(b, json!({"action": "tabs"})));
    assert_eq!(
        tab_ids(&go(a, json!({"action": "tabs"}))),
        ta,
        "a popup belongs to the tab whose page opened it"
    );
    let (popup_target, _) = page_of(b, tb2[1]);
    assert_eq!(lk(&chrome.book).owner_of_target(&popup_target), Some(b));
    assert_eq!(lk(&chrome.book).owned(a), 1);
    assert_eq!(lk(&chrome.book).owned(b), 2);
    assert_eq!(live_pages(), 3);
    let v = go(a, json!({"action": "switch_tab", "id": tb2[1]}));
    assert_eq!(v["ok"], json!(false), "{v}");
    let v = go(
        a,
        json!({"action": "eval", "expression": "window.open('/three') && 1"}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    wait_for("the popup of tab A", || {
        go(a, json!({"action": "status"}))["title"] == "Third"
    });
    assert_eq!(tab_ids(&go(a, json!({"action": "tabs"}))).len(), 2);
    assert_eq!(tab_ids(&go(b, json!({"action": "tabs"}))), tb2);

    let v = go(a, json!({"action": "close"}));
    assert_eq!(v["closed"], json!(true), "{v}");
    assert_eq!(live_uids(), vec![b]);
    wait_for("tab A's pages to close", || live_pages() == 2);
    let mut ended = false;
    let end = Instant::now() + Duration::from_secs(3);
    while !ended && Instant::now() < end {
        if let Some(Out::Text(t)) = oa.pop(Duration::from_millis(100)) {
            ended = t.contains("\"ended\"");
        }
    }
    assert!(ended, "tab A's pane is told its pages ended");
    assert_eq!(browser_pid(), Some(pid));
    let v = go(b, json!({"action": "text"}));
    assert!(
        v["text"].as_str().unwrap().contains("page two"),
        "closing one tab leaves the other's pages: {v}"
    );
    assert_eq!(tab_ids(&go(b, json!({"action": "tabs"}))), tb2);
    assert_eq!(go(a, json!({"action": "status"}))["open"], json!(false));

    reap(&[]);
    assert!(live_uids().is_empty());
    wait_for("all pages to close", || live_pages() == 0);
    housekeep();
    assert_eq!(browser_pid(), Some(pid), "it lingers before it exits");
    set_linger(Duration::ZERO);
    housekeep();
    assert_eq!(browser_pid(), None);
    wait_for("the browser to exit", || {
        !alive(pid) && profiles().is_empty()
    });
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn the_page_limit_counts_awake_pages_over_all_tabs() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if find_chrome("").is_none() {
        eprintln!("no Chrome here: skipped");
        return;
    }
    let _chrome = OneChrome::new();
    let (port, stop) = site();
    let base = format!("http://127.0.0.1:{port}");
    let (a, b) = (987_111, 987_112);
    set_page_cap(6);
    let v = go(a, json!({"action": "navigate", "url": format!("{base}/")}));
    assert_eq!(v["ok"], json!(true), "{v}");
    for _ in 0..3 {
        let v = go(a, json!({"action": "new_tab"}));
        assert_eq!(v["ok"], json!(true), "{v}");
    }
    let v = go(
        b,
        json!({"action": "navigate", "url": format!("{base}/three")}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    let v = go(b, json!({"action": "new_tab"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(live_pages(), 6);
    let before = tab_ids(&go(b, json!({"action": "tabs"})));

    let v = go(b, json!({"action": "new_tab"}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("6 pages awake"),
        "{v}"
    );
    assert_eq!(
        tab_ids(&go(b, json!({"action": "tabs"}))),
        before,
        "a refused tab leaves no entry behind"
    );
    assert_eq!(go(b, json!({"action": "tabs"}))["active"], json!(before[1]));
    let v = go(b, json!({"action": "switch_tab", "id": before[0]}));
    assert_eq!(v["title"], "Third", "{v}");
    let v = go(
        b,
        json!({"action": "eval", "expression": "window.open('/two') ? 1 : 0"}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    nap(1200);
    assert_eq!(live_pages(), 6, "a popup over the limit is closed again");
    assert_eq!(tab_ids(&go(b, json!({"action": "tabs"}))), before);
    let v = go(b, json!({"action": "console"}));
    assert!(
        serde_json::to_string(&v["entries"])
            .unwrap()
            .contains("new tab was refused"),
        "{v}"
    );
    let c = 987_113;
    let v = go(c, json!({"action": "navigate", "url": format!("{base}/")}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert_eq!(
        live_uids(),
        vec![a, b],
        "a tab that got no page is not kept"
    );

    let ta = tab_ids(&go(a, json!({"action": "tabs"})));
    assert!(get(a).unwrap().sleep_page(ta[1] as u32));
    wait_for("the sleeping page to free its place", || live_pages() == 5);
    let v = go(
        b,
        json!({"action": "new_tab", "url": format!("{base}/two")}),
    );
    assert_eq!(v["ok"], json!(true), "a sleeping page does not count: {v}");
    assert_eq!(v["title"], "Second", "{v}");
    let v = go(a, json!({"action": "switch_tab", "id": ta[1]}));
    assert_eq!(v["ok"], json!(false), "waking needs a free place too: {v}");
    let v = go(a, json!({"action": "new_tab"}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("6 pages")
            || v["error"].as_str().unwrap().contains("6 tabs"),
        "{v}"
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn idle_pages_sleep_and_wake_as_themselves() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if find_chrome("").is_none() {
        eprintln!("no Chrome here: skipped");
        return;
    }
    let _chrome = OneChrome::new();
    let (port, stop) = site();
    let base = format!("http://127.0.0.1:{port}");
    let a = 987_121;
    let v = go(
        a,
        json!({"action": "navigate", "url": format!("{base}/two"), "width": 800, "height": 600}),
    );
    assert_eq!(v["title"], "Second", "{v}");
    let v = go(a, json!({"action": "scroll", "selector": "#end"}));
    let y = v["scroll"]["y"].as_i64().unwrap();
    assert!(y > 1000, "{v}");
    let v = go(
        a,
        json!({"action": "eval", "expression": "console.log('before the sleep'); 1"}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    let v = go(a, json!({"action": "new_tab", "url": format!("{base}/")}));
    assert_eq!(v["title"], "Home page", "{v}");
    let ids = tab_ids(&go(a, json!({"action": "tabs"})));
    let (first, second) = (ids[0], ids[1]);
    let pid = browser_pid().unwrap();
    let dir = profiles().remove(0);

    housekeep();
    let v = go(a, json!({"action": "tabs"}));
    assert!(
        v["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t.get("asleep").is_none()),
        "nothing sleeps inside the window: {v}"
    );

    let out = Outbox::new();
    let r = ask(a, 5, &out, "o1", json!({"op": "open", "w": 800, "h": 600}));
    assert_eq!(r["ok"], json!(true), "{r}");
    set_sleep_after(Duration::from_millis(300));
    nap(900);
    let awake = rss_mb(&dir);
    housekeep();
    let v = go(a, json!({"action": "tabs"}));
    assert_eq!(
        tab_ids(&v),
        ids,
        "the sleeping tab keeps its id and place: {v}"
    );
    assert_eq!(v["tabs"][0]["asleep"], json!(true), "{v}");
    assert!(
        v["tabs"][0]["url"].as_str().unwrap().ends_with("/two"),
        "{v}"
    );
    assert_eq!(v["tabs"][0]["title"], "Second", "{v}");
    assert!(
        v["tabs"][1].get("asleep").is_none(),
        "the page shown in a pane is never put to sleep: {v}"
    );
    assert_eq!(v["active"], json!(second));
    wait_for("the sleeping page to leave the browser", || {
        live_pages() == 1
    });
    nap(1200);
    let asleep = rss_mb(&dir);
    eprintln!(
        "MEMORY sleep: browser tree {awake:.0} MB with two pages awake, {asleep:.0} MB with one asleep, {:.0} MB freed",
        awake - asleep
    );
    let v = go(a, json!({"action": "status"}));
    assert_eq!(
        (v["owned_pages"].clone(), v["asleep_pages"].clone()),
        (json!(2), json!(1)),
        "{v}"
    );
    assert!(v.get("asleep").is_none() && v.get("woke").is_none(), "{v}");
    let r = ask(a, 5, &out, "k1", json!({"op": "keep"}));
    assert_eq!(r["ok"], json!(true), "{r}");

    set_sleep_after(Duration::from_secs(600));
    let v = go(a, json!({"action": "console"}));
    assert!(v.get("woke").is_none(), "{v}");
    let r = ask(a, 5, &out, "s1", json!({"op": "tab_switch", "page": first}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["active"], json!(first), "{r}");
    assert_eq!(tab_ids(&r), ids, "{r}");
    assert!(
        r["tabs"][0].get("asleep").is_none(),
        "clicking a sleeping tab wakes it: {r}"
    );
    assert!(r["url"].as_str().unwrap().ends_with("/two"), "{r}");
    let v = go(a, json!({"action": "text"}));
    assert!(v["text"].as_str().unwrap().contains("page two"), "{v}");
    assert_eq!(
        v["woke"],
        json!(true),
        "the agent is told once that the page was reloaded: {v}"
    );
    assert!(v["note"].as_str().unwrap().contains("put to sleep"), "{v}");
    let v = go(
        a,
        json!({"action": "eval", "expression": "Math.round(scrollY)"}),
    );
    assert!(
        v.get("woke").is_none() && v.get("note").is_none(),
        "only once: {v}"
    );
    assert!(
        (v["result"].as_i64().unwrap() - y).abs() < 5,
        "the scroll position is restored: {v} (was {y})"
    );
    let v = go(a, json!({"action": "console"}));
    assert!(
        serde_json::to_string(&v["entries"])
            .unwrap()
            .contains("before the sleep"),
        "the console of a page survives its sleep: {v}"
    );
    assert_eq!(picture(&out), Some(a), "the pane shows the woken page");

    let r = ask(a, 5, &out, "c1", json!({"op": "close"}));
    assert_eq!(r["ok"], json!(true), "{r}");
    set_sleep_after(Duration::from_millis(300));
    nap(700);
    housekeep();
    let v = go(a, json!({"action": "tabs"}));
    assert_eq!(tab_ids(&v), ids, "{v}");
    assert!(
        v["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["asleep"] == json!(true)),
        "without a pane the active page sleeps too, last: {v}"
    );
    assert_eq!(v["active"], json!(first), "{v}");
    let v = go(a, json!({"action": "status"}));
    assert_eq!(v["open"], json!(true), "{v}");
    assert_eq!(v["asleep"], json!(true), "{v}");
    assert_eq!(
        (v["owned_pages"].clone(), v["asleep_pages"].clone()),
        (json!(2), json!(2)),
        "{v}"
    );
    assert!(v["url"].as_str().unwrap().ends_with("/two"), "{v}");
    wait_for("no awake pages", || live_pages() == 0);
    set_linger(Duration::ZERO);
    housekeep();
    assert_eq!(
        browser_pid(),
        None,
        "the browser exits when every page sleeps"
    );
    wait_for("the browser to exit", || !alive(pid));
    assert_eq!(live_uids(), vec![a], "the tab keeps its sleeping page tabs");
    set_linger(Duration::from_secs(180));
    set_sleep_after(Duration::from_secs(600));

    let v = go(a, json!({"action": "text"}));
    assert!(
        v["text"].as_str().unwrap().contains("page two"),
        "an action starts the browser again and wakes the page: {v}"
    );
    assert_eq!(v["woke"], json!(true), "{v}");
    let again = browser_pid().expect("a new browser");
    assert_ne!(again, pid);
    let v = go(a, json!({"action": "tabs"}));
    assert_eq!(tab_ids(&v), ids, "{v}");
    assert!(v["tabs"][0].get("asleep").is_none(), "{v}");
    assert_eq!(
        v["tabs"][1]["asleep"],
        json!(true),
        "only the page that was used woke: {v}"
    );
    assert_eq!(live_pages(), 1);
    let v = go(a, json!({"action": "switch_tab", "id": second}));
    assert_eq!(v["title"], "Home page", "{v}");
    assert_eq!(v["woke"], json!(true), "{v}");

    let s = get(a).unwrap();
    assert!(s.sleep_page(second as u32));
    let v = go(a, json!({"action": "status"}));
    assert_eq!(v["asleep"], json!(true), "{v}");
    let r = ask(a, 6, &out, "o2", json!({"op": "open", "w": 800, "h": 600}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["active"], json!(second), "{r}");
    assert_eq!(tab_ids(&r), ids, "{r}");
    assert!(
        r["tabs"][1].get("asleep").is_none(),
        "showing the pane wakes the page it shows: {r}"
    );
    wait_for("the woken page to load", || {
        go(a, json!({"action": "status"}))["title"] == "Home page"
    });
    assert_eq!(picture(&out), Some(a));
    let v = go(a, json!({"action": "close_tab", "id": first}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(tab_ids(&v), vec![second], "{v}");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn a_killed_browser_is_started_again_on_the_next_use() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if find_chrome("").is_none() {
        eprintln!("no Chrome here: skipped");
        return;
    }
    let _chrome = OneChrome::new();
    let (port, stop) = site();
    let base = format!("http://127.0.0.1:{port}");
    let (a, b) = (987_131, 987_132);
    let v = go(a, json!({"action": "navigate", "url": format!("{base}/")}));
    assert_eq!(v["title"], "Home page", "{v}");
    let v = go(
        b,
        json!({"action": "navigate", "url": format!("{base}/three")}),
    );
    assert_eq!(v["title"], "Third", "{v}");
    let ta = tab_ids(&go(a, json!({"action": "tabs"})));
    let pid = browser_pid().unwrap();
    let dir = profiles().remove(0);
    assert_eq!(
        crate::platform::kill(pid as i32, crate::platform::SIGKILL),
        0
    );
    wait_for("the dead browser to be noticed", || browser_pid().is_none());
    housekeep();
    assert_eq!(live_uids(), vec![a, b], "the tabs keep their page tabs");
    let v = go(a, json!({"action": "tabs"}));
    assert_eq!(tab_ids(&v), ta, "{v}");
    assert_eq!(v["tabs"][0]["asleep"], json!(true), "{v}");
    assert!(
        v["tabs"][0]["url"].as_str().unwrap().starts_with(&base),
        "{v}"
    );
    wait_for("the old profile to go", || !dir.exists());

    let v = go(a, json!({"action": "text"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(v["text"].as_str().unwrap().contains("Welcome home"), "{v}");
    assert_eq!(v["woke"], json!(true), "{v}");
    assert!(
        v["note"].as_str().unwrap().contains("stopped unexpectedly"),
        "{v}"
    );
    let again = browser_pid().expect("a new browser");
    assert_ne!(again, pid);
    assert!(alive(again));
    let v = go(a, json!({"action": "console"}));
    assert!(
        serde_json::to_string(&v["entries"])
            .unwrap()
            .contains("stopped unexpectedly"),
        "{v}"
    );
    let v = go(a, json!({"action": "status"}));
    assert!(v.get("note").is_none(), "{v}");
    let v = go(b, json!({"action": "status"}));
    assert_eq!(v["asleep"], json!(true), "{v}");
    let v = go(b, json!({"action": "eval", "expression": "document.title"}));
    assert_eq!(v["result"], "Third", "{v}");
    assert!(
        v["note"].as_str().unwrap().contains("stopped unexpectedly"),
        "every tab that lost pages is told: {v}"
    );
    assert_eq!(browser_pid(), Some(again), "one browser for both again");
    assert_eq!(live_pages(), 2);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
#[ignore]
fn memory_of_four_tabs_in_one_browser() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if find_chrome("").is_none() {
        return;
    }
    let _chrome = OneChrome::new();
    let (port, stop) = site();
    let base = format!("http://127.0.0.1:{port}");
    for uid in 987_141..987_145u64 {
        let v = go(
            uid,
            json!({"action": "navigate", "url": format!("{base}/")}),
        );
        assert_eq!(v["title"], "Home page", "{v}");
    }
    let dir = profiles().remove(0);
    nap(6000);
    let four = rss_mb(&dir);
    let s = get(987_144).unwrap();
    let id = lk(&s.st).tabs.active;
    assert!(s.sleep_page(id));
    nap(4000);
    let three = rss_mb(&dir);
    for uid in 987_141..987_144u64 {
        let s = get(uid).unwrap();
        let id = lk(&s.st).tabs.active;
        assert!(s.sleep_page(id));
    }
    nap(4000);
    let none = rss_mb(&dir);
    eprintln!("MEMORY shared: 4 tabs with one page each {four:.0} MB; after one page sleeps {three:.0} MB ({:.0} MB freed); all asleep {none:.0} MB", four - three);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}
