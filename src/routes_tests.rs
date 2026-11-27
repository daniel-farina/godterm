use crate::app::App;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::control::tab_id;
use crate::pane::LaunchKind;
use crate::routes::{self, ProxyUrl, RouteCfg, RoutesCfg, Scheme};
use crate::routes_ts::{self, Instance, TsState};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SECRET: &str = "s3cr3t-pass-9";

fn short_home(tag: &str) -> PathBuf {
    let base = if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let home = base.join(format!("gt-rt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    home
}

fn script(path: &Path, body: &str) -> String {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_string_lossy().into_owned()
}

fn fake_agent(home: &Path) -> String {
    script(
        &home.join("fake-agent"),
        "#!/bin/sh\nd=$(dirname \"$0\")\nenv > \"$d/env.tmp\"\nmv \"$d/env.tmp\" \"$d/env.out\"\nsleep 30\n",
    )
}

fn test_app(tag: &str) -> (App, PathBuf) {
    let home = short_home(tag);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut cfg = crate::config::Config::default();
    cfg.claude_bin = Some(fake_agent(&home));
    cfg.save().unwrap();
    (App::new(cfg, tx), home)
}

fn web(app: &mut App, mut args: Value) -> Value {
    args["_client"] = json!("web");
    app.control_call("tab_route", &args)
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(10), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn launched_env(home: &Path) -> Vec<(String, String)> {
    let p = home.join("env.out");
    wait_for("the fake agent wrote its environment", || p.exists());
    let text = std::fs::read_to_string(&p).unwrap();
    let _ = std::fs::remove_file(&p);
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn var<'a>(env: &'a [(String, String)], k: &str) -> Option<&'a str> {
    env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

fn kill_all(app: &mut App, home: &Path) {
    for p in &mut app.panes {
        p.kill_all();
    }
    app.routes_shutdown();
    let _ = std::fs::remove_dir_all(home);
}

fn listener() -> (TcpListener, u16) {
    let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    (l, port)
}

fn closed_port() -> u16 {
    listener().1
}

#[test]
fn proxy_urls_parse_and_mask_their_credentials() {
    let u = ProxyUrl::parse("http://alice:p%40ss@proxy.example:3128/").unwrap();
    assert_eq!(u.scheme, Scheme::Http);
    assert_eq!((u.host.as_str(), u.port), ("proxy.example", 3128));
    assert_eq!(
        (u.user.as_deref(), u.pass.as_deref()),
        (Some("alice"), Some("p@ss"))
    );
    assert_eq!(u.masked(), "http://***@proxy.example:3128");
    assert_eq!(format!("{u:?}"), "http://***@proxy.example:3128");
    assert_eq!(
        routes::mask_url("socks5h://[::1]:1080"),
        "socks5h://[::1]:1080"
    );
    assert_eq!(ProxyUrl::parse("socks5://h").unwrap().port, 1080);
    assert_eq!(ProxyUrl::parse("https://h").unwrap().port, 443);
    for bad in [
        "",
        "proxy.example:3128",
        "ftp://h:1",
        "http://",
        "http://h:0",
        "http://h/path",
        "http://h :1",
    ] {
        assert!(ProxyUrl::parse(bad).is_err(), "{bad}");
    }
    let r = RouteCfg {
        name: "office".into(),
        kind: "custom".into(),
        url: format!("http://alice:{SECRET}@proxy.example:3128"),
        ..Default::default()
    };
    assert!(!format!("{r:?}").contains(SECRET));
    let cfg = RoutesCfg {
        list: vec![r],
        ..Default::default()
    };
    assert!(!format!("{cfg:?}").contains(SECRET));
    for n in ["office", "exit-1", "a_b"] {
        assert!(routes::valid_name(n), "{n}");
    }
    for n in [
        "",
        "Direct",
        "direct",
        "has space",
        "-x",
        "a/b",
        "abcdefghijklmnopqrstuvwxyz",
    ] {
        assert!(!routes::valid_name(n), "{n}");
    }
}

#[test]
fn the_proxy_variables_keep_the_users_no_proxy() {
    assert_eq!(routes::no_proxy(None), "localhost,127.0.0.1,::1");
    assert_eq!(
        routes::no_proxy(Some("internal.example, LOCALHOST .corp.example")),
        "localhost,127.0.0.1,::1,internal.example,.corp.example"
    );
    let env = routes::proxy_env(
        "http://127.0.0.1:1",
        "socks5h://127.0.0.1:1",
        Some("x.example"),
    );
    let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    for k in routes::PROXY_VARS {
        assert!(keys.contains(&k), "{k}");
    }
    assert_eq!(var(&env, "HTTPS_PROXY"), Some("http://127.0.0.1:1"));
    assert_eq!(var(&env, "all_proxy"), Some("socks5h://127.0.0.1:1"));
    assert_eq!(
        var(&env, "no_proxy"),
        Some("localhost,127.0.0.1,::1,x.example")
    );
}

fn http_proxy() -> (u16, std::sync::mpsc::Receiver<String>) {
    let (l, port) = listener();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { break };
            let mut head = vec![];
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if c.read(&mut b).unwrap_or(0) == 0 {
                    break;
                }
                head.push(b[0]);
            }
            let _ = tx.send(String::from_utf8_lossy(&head).into_owned());
            let _ = c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
        }
    });
    (port, rx)
}

fn socks_proxy(user: &'static str, pass: &'static str) -> (u16, std::sync::mpsc::Receiver<String>) {
    let (l, port) = listener();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { break };
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut h = [0u8; 2];
                if c.read_exact(&mut h).is_err() {
                    return;
                }
                let mut methods = vec![0u8; h[1] as usize];
                c.read_exact(&mut methods).unwrap();
                if !methods.contains(&2) {
                    let _ = c.write_all(&[5, 0xff]);
                    return;
                }
                c.write_all(&[5, 2]).unwrap();
                let mut a = [0u8; 2];
                c.read_exact(&mut a).unwrap();
                let mut u = vec![0u8; a[1] as usize];
                c.read_exact(&mut u).unwrap();
                let mut n = [0u8; 1];
                c.read_exact(&mut n).unwrap();
                let mut p = vec![0u8; n[0] as usize];
                c.read_exact(&mut p).unwrap();
                let ok = u == user.as_bytes() && p == pass.as_bytes();
                c.write_all(&[1, if ok { 0 } else { 1 }]).unwrap();
                if !ok {
                    return;
                }
                let mut r = [0u8; 5];
                c.read_exact(&mut r).unwrap();
                assert_eq!(r[3], 3, "a socks5h route sends the name, not an address");
                let mut host = vec![0u8; r[4] as usize + 2];
                c.read_exact(&mut host).unwrap();
                let name = String::from_utf8_lossy(&host[..host.len() - 2]).into_owned();
                let _ = tx.send(name);
                c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
                let mut buf = [0u8; 256];
                while let Ok(k) = c.read(&mut buf) {
                    if k == 0 || c.write_all(&buf[..k]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    (port, rx)
}

#[test]
fn a_tunnel_through_an_http_proxy_sends_the_login_only_to_the_proxy() {
    let (port, heads) = http_proxy();
    let u = ProxyUrl::parse(&format!("http://alice:{SECRET}@127.0.0.1:{port}")).unwrap();
    routes::tunnel_only(&u, "api.example.test", 443).unwrap();
    let head = heads.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        head.starts_with("CONNECT api.example.test:443 HTTP/1.1\r\n"),
        "{head}"
    );
    assert!(
        head.contains("Proxy-Authorization: Basic YWxpY2U6czNjcjN0LXBhc3MtOQ==\r\n"),
        "{head}"
    );
    let p = routes::probe(&u, "api.example.test");
    assert!(!p.ok && !p.detail.contains(SECRET), "{p:?}");
    let down = ProxyUrl::parse(&format!("http://127.0.0.1:{}", closed_port())).unwrap();
    assert!(routes::tunnel_only(&down, "api.example.test", 443).is_err());
}

#[test]
fn a_socks_route_is_bridged_to_a_local_http_proxy() {
    let (port, names) = socks_proxy("bob", SECRET);
    let u = ProxyUrl::parse(&format!("socks5h://bob:{SECRET}@127.0.0.1:{port}")).unwrap();
    routes::tunnel_only(&u, "api.example.test", 443).unwrap();
    assert_eq!(
        names.recv_timeout(Duration::from_secs(5)).unwrap(),
        "api.example.test"
    );
    let wrong = ProxyUrl::parse(&format!("socks5h://bob:nope@127.0.0.1:{port}")).unwrap();
    assert!(routes::tunnel_only(&wrong, "api.example.test", 443).is_err());

    let bridge = routes::Bridge::start(u).unwrap();
    let mut c = TcpStream::connect(("127.0.0.1", bridge.port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.write_all(b"CONNECT api.example.test:443 HTTP/1.1\r\nHost: api.example.test:443\r\n\r\n")
        .unwrap();
    let mut reply = [0u8; 39];
    c.read_exact(&mut reply).unwrap();
    assert!(
        reply.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&reply)
    );
    assert_eq!(
        names.recv_timeout(Duration::from_secs(5)).unwrap(),
        "api.example.test"
    );
    c.write_all(b"ping").unwrap();
    let mut echo = [0u8; 4];
    c.read_exact(&mut echo).unwrap();
    assert_eq!(&echo, b"ping");
}

#[test]
fn a_routed_tab_launches_with_the_proxy_variables_and_an_unrouted_one_without() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("env");
    let (_keep, port) = listener();
    let url = format!("http://127.0.0.1:{port}");
    let r = web(
        &mut app,
        json!({"op": "save", "name": "office", "kind": "custom", "url": url}),
    );
    assert_eq!(r["ok"], true, "{r}");

    app.launch(0, LaunchKind::Normal);
    let env = launched_env(&home);
    let inherited = |k: &str| std::env::var(k).ok();
    for k in routes::PROXY_VARS {
        assert_eq!(
            var(&env, k).map(str::to_string),
            inherited(k),
            "{k} of an unrouted tab"
        );
    }
    assert!(app.tab_json(0, 0).get("route").is_none());

    let uid = app.panes[0].tabs[0].uid;
    let r = web(
        &mut app,
        json!({"op": "set", "tab": tab_id(uid), "route": "office"}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert!(app.panes[0].tabs[0].is_running(), "it was relaunched");
    let env = launched_env(&home);
    for k in [
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
    ] {
        assert_eq!(var(&env, k), Some(url.as_str()), "{k}");
    }
    let np = var(&env, "NO_PROXY").unwrap();
    assert!(np.starts_with("localhost,127.0.0.1,::1"), "{np}");
    assert_eq!(var(&env, "no_proxy"), Some(np));
    let j = app.tab_json(0, 0);
    assert_eq!(
        j["route"],
        json!({"name": "office", "kind": "custom", "state": "on"})
    );

    let r = web(
        &mut app,
        json!({"op": "set", "tab": tab_id(uid), "route": ""}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let env = launched_env(&home);
    assert_eq!(
        var(&env, "HTTPS_PROXY").map(str::to_string),
        inherited("HTTPS_PROXY")
    );
    assert!(app.panes[0].tabs[0].route.is_none());
    kill_all(&mut app, &home);
}

#[test]
fn a_tabs_route_survives_save_load_close_and_resume() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("keep");
    let (_keep, port) = listener();
    let url = format!("http://127.0.0.1:{port}");
    web(
        &mut app,
        json!({"op": "save", "name": "office", "kind": "custom", "url": url}),
    );
    let uid = app.panes[0].tabs[0].uid;
    let r = web(
        &mut app,
        json!({"op": "set", "tab": tab_id(uid), "route": "office"}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(
        r["result"]["say"]
            .as_str()
            .unwrap_or("")
            .contains("will start through office"),
        true,
        "{r}"
    );
    assert!(
        !app.panes[0].tabs[0].is_running(),
        "a tab that was not running is not started"
    );
    app.panes[0].tabs[0].session_id = Some("11111111-2222-4333-8444-555555555555".into());
    app.save_state();

    let (tx, _rx) = std::sync::mpsc::channel();
    let cfg = crate::config::Config::parse(
        &std::fs::read_to_string(crate::config::Config::path()).unwrap(),
    )
    .unwrap();
    assert_eq!(cfg.routes.list.len(), 1);
    let mut next = App::new(cfg, tx);
    next.restore(crate::state::AppState::load().unwrap());
    let (s, t) = next.locate_uid(uid).expect("the tab is back");
    assert_eq!(next.panes[s].tabs[t].route.as_deref(), Some("office"));

    let kind = next.panes[s].tabs[t].relaunch_kind();
    assert!(matches!(kind, LaunchKind::Resume(_)));
    next.launch_tab(s, t, kind);
    let env = launched_env(&home);
    assert_eq!(
        var(&env, "HTTPS_PROXY"),
        Some(url.as_str()),
        "the resumed tab is routed"
    );

    let closed = next.snapshot_tab(s, t, 0).expect("a closed record");
    assert_eq!(closed.route.as_deref(), Some("office"));
    let text = serde_json::to_string(&closed).unwrap();
    let back: crate::closed::ClosedTab = serde_json::from_str(&text).unwrap();
    assert_eq!(back.route.as_deref(), Some("office"));
    kill_all(&mut next, &home);
}

#[test]
fn launch_is_blocked_when_the_proxy_is_down_until_the_user_overrides() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("block");
    let url = format!("http://127.0.0.1:{}", closed_port());
    web(
        &mut app,
        json!({"op": "save", "name": "office", "kind": "custom", "url": url}),
    );
    let uid = app.panes[0].tabs[0].uid;
    web(
        &mut app,
        json!({"op": "set", "tab": tab_id(uid), "route": "office"}),
    );

    app.launch(0, LaunchKind::Normal);
    let tab = &app.panes[0].tabs[0];
    assert!(!tab.is_running(), "never launched unrouted");
    assert!(tab.route_block.is_some());
    assert!(!home.join("env.out").exists());
    let j = app.tab_json(0, 0);
    assert_eq!(j["route"]["state"], "blocked", "{j}");
    assert!(!j.to_string().contains("127.0.0.1"), "{j}");

    let r = app.control_call(
        "reload_tab",
        &json!({"_client": "brain", "id": tab_id(uid)}),
    );
    assert!(
        !app.panes[0].tabs[0].is_running(),
        "an agent's reload does not get past the block: {r}"
    );
    for client in ["brain", "external"] {
        let r = app.control_call(
            "tab_route",
            &json!({"_client": client, "op": "launch_direct", "tab": tab_id(uid)}),
        );
        assert_eq!(r["ok"], false, "{r}");
    }
    assert!(!app.panes[0].tabs[0].is_running());

    let r = web(&mut app, json!({"op": "retry", "tab": tab_id(uid)}));
    assert!(
        r["result"]["say"]
            .as_str()
            .unwrap()
            .starts_with("Still not started"),
        "{r}"
    );
    assert!(!app.panes[0].tabs[0].is_running());

    let r = web(&mut app, json!({"op": "launch_direct", "tab": tab_id(uid)}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(app.panes[0].tabs[0].is_running());
    let env = launched_env(&home);
    assert_eq!(
        var(&env, "HTTPS_PROXY").map(str::to_string),
        std::env::var("HTTPS_PROXY").ok()
    );
    assert_eq!(app.tab_json(0, 0)["route"]["state"], "bypassed");
    assert_eq!(
        app.panes[0].tabs[0].route.as_deref(),
        Some("office"),
        "the route stays set"
    );

    app.panes[0].tabs[0].kill();
    app.launch(0, LaunchKind::Normal);
    assert!(
        !app.panes[0].tabs[0].is_running(),
        "the override was for one launch only"
    );
    let log = std::fs::read_to_string(crate::log::path()).unwrap_or_default();
    assert!(
        log.contains("launches without its route office: the user chose to"),
        "{log}"
    );
    kill_all(&mut app, &home);
}

#[test]
fn credentials_in_a_custom_url_never_reach_logs_or_state() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("secret");
    let (_keep, port) = listener();
    let url = format!("http://alice:{SECRET}@127.0.0.1:{port}");
    let r = web(
        &mut app,
        json!({"op": "save", "name": "office", "kind": "custom", "url": url}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert!(!r.to_string().contains(SECRET), "{r}");
    assert_eq!(
        r["result"]["view"]["routes"][0]["url"],
        format!("http://***@127.0.0.1:{port}")
    );
    assert_eq!(r["result"]["view"]["routes"][0]["has_secret"], true);

    let uid = app.panes[0].tabs[0].uid;
    app.launch(0, LaunchKind::Normal);
    let _ = launched_env(&home);
    let r = web(
        &mut app,
        json!({"op": "set", "tab": tab_id(uid), "route": "office", "force": true}),
    );
    assert!(!r.to_string().contains(SECRET), "{r}");
    let env = launched_env(&home);
    assert_eq!(
        var(&env, "HTTPS_PROXY"),
        Some(url.as_str()),
        "the tab's own process gets the full URL"
    );

    let r = web(
        &mut app,
        json!({"op": "save", "name": "office", "was": "office", "kind": "custom", "url": ""}),
    );
    assert_eq!(
        r["ok"], true,
        "an edit without a URL keeps the saved one: {r}"
    );
    assert_eq!(app.cfg.routes.list[0].url, url);
    let bad = web(
        &mut app,
        json!({"op": "save", "name": "other", "kind": "custom", "url": format!("ftp://alice:{SECRET}@h")}),
    );
    assert!(
        bad["ok"] == false && !bad.to_string().contains(SECRET),
        "{bad}"
    );

    let mut seen = String::new();
    seen.push_str(&web(&mut app, json!({"op": "view"})).to_string());
    seen.push_str(&app.tab_json(0, 0).to_string());
    for client in ["brain", "external", "web"] {
        seen.push_str(
            &app.control_call("get_state", &json!({"_client": client}))
                .to_string(),
        );
    }
    seen.push_str(
        &app.control_call(
            "list_settings",
            &json!({"_client": "brain", "query": "route"}),
        )
        .to_string(),
    );
    seen.push_str(
        &app.control_call(
            "get_setting",
            &json!({"_client": "brain", "key": "routes.list"}),
        )
        .to_string(),
    );
    seen.push_str(&format!("{:?}", app.cfg.routes));
    seen.push_str(&std::fs::read_to_string(crate::log::path()).unwrap_or_default());
    assert!(!seen.contains(SECRET), "the password leaked");
    assert!(seen.contains("office"));
    let cfg = std::fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(cfg.contains(SECRET), "it is stored only in config.toml");
    for key in ["routes", "routes.list", "routes.tailscaled_bin"] {
        assert!(
            crate::sync::portable::is_local_key(key, &[]),
            "{key} never leaves this machine with sync"
        );
    }
    let portable = crate::sync::portable::flatten(
        &cfg,
        &crate::sync::portable::PathMap {
            home: home.clone(),
            named: vec![],
        },
        &[],
    )
    .unwrap();
    assert!(!format!("{portable:?}").contains(SECRET));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(crate::config::Config::path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "config.toml is private");
    }
    kill_all(&mut app, &home);
}

#[test]
fn agents_cannot_set_clear_or_edit_routes() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("agents");
    let (_keep, port) = listener();
    web(
        &mut app,
        json!({"op": "save", "name": "office", "kind": "custom", "url": format!("http://127.0.0.1:{port}")}),
    );
    let uid = app.panes[0].tabs[0].uid;
    web(
        &mut app,
        json!({"op": "set", "tab": tab_id(uid), "route": "office"}),
    );
    let tab = tab_id(uid);
    let attempts = [
        json!({"op": "set", "tab": tab, "route": ""}),
        json!({"op": "set", "tab": tab, "route": "office"}),
        json!({"op": "save", "name": "mine", "kind": "custom", "url": "http://127.0.0.1:9"}),
        json!({"op": "delete", "name": "office"}),
        json!({"op": "bins", "tailscaled": "/bin/sh"}),
        json!({"op": "view"}),
        json!({"op": "test", "name": "office"}),
    ];
    for client in [
        Some("brain".to_string()),
        Some("external".to_string()),
        Some(format!("tab:{uid}")),
        Some("ui".to_string()),
        None,
    ] {
        for a in &attempts {
            let mut a = a.clone();
            if let Some(c) = &client {
                a["_client"] = json!(c);
            }
            let r = app.control_call("tab_route", &a);
            assert_eq!(r["ok"], false, "{client:?} {a}: {r}");
            assert!(r["error"].as_str().unwrap().starts_with("refused"), "{r}");
        }
    }
    assert_eq!(app.panes[0].tabs[0].route.as_deref(), Some("office"));
    assert_eq!(app.cfg.routes.list.len(), 1);
    assert!(app.cfg.routes.tailscaled_bin.is_empty());
    assert!(
        !crate::control::TOOLS.iter().any(|t| t.name == "tab_route"),
        "it is not in the tool list any agent sees"
    );
    let state = app
        .control_call("get_state", &json!({"_client": "brain"}))
        .to_string();
    assert!(
        state.contains("\"route\""),
        "an agent is told the tab is routed"
    );

    let busy = web(&mut app, json!({"op": "delete", "name": "office"}));
    assert!(
        busy["error"].as_str().unwrap().contains("1 tab use office"),
        "{busy}"
    );
    kill_all(&mut app, &home);
}

const FAKE_DAEMON: &str = r#"#!/bin/sh
dir=""
sock=""
for a in "$@"; do
  case "$a" in
    --statedir=*) dir="${a#--statedir=}" ;;
    --socket=*) sock="${a#--socket=}" ;;
  esac
done
echo "$@" >> "$dir/daemon.log"
: > "$sock"
trap 'rm -f "$sock"; exit 0' TERM INT
while :; do sleep 0.05; done
"#;

const FAKE_CLI: &str = r#"#!/bin/sh
echo "$@" >> "$(dirname "$0")/cli.log"
sock=""
case "$1" in --socket=*) sock="${1#--socket=}"; shift ;; esac
if [ -z "$sock" ]; then
  if [ "$1" = "exit-node" ]; then
    printf ' IP           HOSTNAME                     COUNTRY  CITY  STATUS\n'
    printf ' 192.0.2.9   exit-node-1.example.ts.net   -        -     -\n'
    printf ' 192.0.2.10  exit-node-2.example.ts.net   -        -     offline\n\n# To use an exit node, use tailscale set\n'
    exit 0
  fi
  exit 1
fi
[ -e "$sock" ] || { echo "failed to connect to local tailscaled" >&2; exit 1; }
dir=$(dirname "$sock")
case "$1" in
  status) cat "$dir/fake-status.json" 2>/dev/null || echo '{"BackendState":"NoState"}' ;;
  up) exit 0 ;;
  set) exit 0 ;;
esac
"#;

const APP_CLI: &str = "#!/bin/sh\necho \"$@\" >> \"$(dirname \"$0\")/cli.log\"\necho '{\"BackendState\":\"Running\"}'\nexit 0\n";

fn fake_bins(home: &Path, cli: &str) -> RoutesCfg {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    RoutesCfg {
        tailscaled_bin: script(&bin.join("tailscaled"), FAKE_DAEMON),
        tailscale_bin: script(&bin.join("tailscale"), cli),
        list: vec![],
    }
}

fn ts_route(node: &str) -> RouteCfg {
    RouteCfg {
        name: "exit".into(),
        kind: "tailscale".into(),
        exit_node: node.into(),
        ..Default::default()
    }
}

fn status(inst: &Instance, v: Value) {
    std::fs::write(inst.dir.join("fake-status.json"), v.to_string()).unwrap();
}

fn peers(active: bool) -> Value {
    json!({
        "nodekey:aaaa": {"HostName": "exit-node-1", "DNSName": "exit-node-1.example.ts.net.", "TailscaleIPs": ["192.0.2.9"], "Online": true, "ExitNodeOption": true, "ExitNode": active},
        "nodekey:bbbb": {"HostName": "exit-node-2", "DNSName": "exit-node-2.example.ts.net.", "TailscaleIPs": ["192.0.2.10"], "Online": false, "ExitNodeOption": true, "ExitNode": false},
        "nodekey:cccc": {"HostName": "laptop", "DNSName": "laptop.example.ts.net.", "TailscaleIPs": ["192.0.2.11"], "Online": true, "ExitNodeOption": false, "ExitNode": false}
    })
}

fn step_ready(inst: &mut Instance) {
    wait_for("the fake tailscaled is up", || inst.sock.exists());
    inst.step();
}

fn lines(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_managed_tailscale_goes_from_missing_to_login_to_ready_and_stops() {
    let home = short_home("ts");
    let missing = RoutesCfg {
        tailscaled_bin: home.join("nope/tailscaled").to_string_lossy().into_owned(),
        ..Default::default()
    };
    let mut inst = Instance::new(&home, ts_route("exit-node-1"), missing);
    inst.step();
    assert!(
        matches!(&inst.state, TsState::Missing(m) if m.contains("brew install tailscale")),
        "{:?}",
        inst.state
    );
    assert_eq!(inst.daemon_pid(), None);
    assert!(matches!(
        routes_ts::resolve(&RoutesCfg::default()),
        Err(m) if m.contains("tailscaled is not installed")
    ));

    let bins = fake_bins(&home, FAKE_CLI);
    let mut inst = Instance::new(&home, ts_route("exit-node-1"), bins);
    let sock = format!("--socket={}", inst.sock.display());
    inst.step();
    assert_eq!(inst.state, TsState::Starting);
    let first_pid = inst.daemon_pid().expect("tailscaled was started");
    let daemon_log = inst.dir.join("daemon.log");
    wait_for("the daemon logged its arguments", || {
        lines(&daemon_log).len() == 1
    });
    let args = lines(&daemon_log)[0].clone();
    assert_eq!(
        args,
        format!(
            "--tun=userspace-networking --statedir={} {sock}",
            inst.dir.display()
        )
    );
    assert!(!inst.proxy, "no proxy port is open during setup");

    status(
        &inst,
        json!({"BackendState": "NeedsLogin", "AuthURL": "https://login.example.test/a/abc123"}),
    );
    step_ready(&mut inst);
    assert_eq!(
        inst.state,
        TsState::Login("https://login.example.test/a/abc123".into())
    );
    assert!(!inst.proxy);

    status(
        &inst,
        json!({"BackendState": "Running", "Peer": peers(false)}),
    );
    step_ready(&mut inst);
    assert_eq!(inst.state, TsState::Starting);
    assert_eq!(
        inst.nodes
            .iter()
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>(),
        ["exit-node-1", "exit-node-2"]
    );
    assert!(!inst.proxy);

    status(
        &inst,
        json!({"BackendState": "Running", "Peer": peers(true), "ExitNodeStatus": {"ID": "n1", "Online": true, "TailscaleIPs": ["192.0.2.9/32"]}}),
    );
    step_ready(&mut inst);
    assert_eq!(
        inst.state,
        TsState::Starting,
        "it restarts with the proxy now that the exit node is active"
    );
    assert!(inst.proxy);
    let port = inst.port;
    assert_ne!(port, 0, "the proxy gets a port only now");
    assert_ne!(inst.daemon_pid(), Some(first_pid));
    assert!(!crate::platform::alive(first_pid));
    wait_for("the second daemon logged its arguments", || {
        lines(&daemon_log).len() == 2
    });
    assert_eq!(
        lines(&daemon_log)[1],
        format!(
            "--tun=userspace-networking --statedir={} {sock} --socks5-server=127.0.0.1:{port} --outbound-http-proxy-listen=127.0.0.1:{port}",
            inst.dir.display()
        )
    );
    step_ready(&mut inst);
    assert_eq!(inst.state, TsState::Ready);

    status(
        &inst,
        json!({"BackendState": "Running", "Peer": peers(true), "ExitNodeStatus": {"ID": "n1", "Online": false}}),
    );
    step_ready(&mut inst);
    assert!(
        matches!(&inst.state, TsState::Unreachable(m) if m.contains("offline")),
        "{:?}",
        inst.state
    );
    assert!(
        !inst.proxy,
        "the proxy is closed when the exit node is not active"
    );
    wait_for("the third daemon logged its arguments", || {
        lines(&daemon_log).len() == 3
    });
    assert!(!lines(&daemon_log)[2].contains("socks5"));

    status(&inst, json!({"BackendState": "NeedsLogin"}));
    step_ready(&mut inst);
    assert!(!inst.proxy);
    assert!(matches!(inst.state, TsState::Starting | TsState::Login(_)));

    let pid = inst.daemon_pid().unwrap();
    inst.stop();
    assert_eq!(inst.state, TsState::Stopped);
    assert!(!crate::platform::alive(pid));
    assert_eq!(inst.daemon_pid(), None);

    let cli = lines(&home.join("bin/cli.log"));
    let host = "--hostname=godterm-exit";
    assert!(
        cli.contains(&format!("{sock} up --reset {host} --json")),
        "{cli:?}"
    );
    assert!(
        cli.contains(&format!(
            "{sock} set --exit-node=exit-node-1 --exit-node-allow-lan-access=false"
        )),
        "{cli:?}"
    );
    for l in &cli {
        assert!(
            l.starts_with(&format!("--socket={}", inst.dir.display())),
            "every command names the route's own socket: {l}"
        );
        let words: Vec<&str> = l.split(' ').collect();
        assert!(matches!(words[1], "status" | "up" | "set"), "{l}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_tailscale_command_that_ignores_the_socket_is_never_used() {
    let home = short_home("tsapp");
    let bins = fake_bins(&home, APP_CLI);
    let mut inst = Instance::new(&home, ts_route("exit-node-1"), bins);
    for _ in 0..3 {
        inst.step();
    }
    assert!(
        matches!(&inst.state, TsState::Missing(m) if m.contains("ignores --socket")),
        "{:?}",
        inst.state
    );
    assert_eq!(inst.daemon_pid(), None, "tailscaled is not even started");
    let cli = lines(&home.join("bin/cli.log"));
    assert!(!cli.is_empty());
    for l in &cli {
        assert!(
            l.ends_with("absent.sock status --json"),
            "only the read-only check ran: {l}"
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_system_tailscale_is_only_asked_for_its_exit_node_list() {
    let home = short_home("tssys");
    let bins = fake_bins(&home, FAKE_CLI);
    let names = routes_ts::system_exit_nodes(&bins, &home.join("tailscale")).unwrap();
    assert_eq!(names, ["exit-node-1", "exit-node-2"]);
    assert_eq!(lines(&home.join("bin/cli.log")), ["exit-node list"]);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_app_starts_the_managed_tailscale_on_demand_and_stops_it_when_unused() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("tsapp2");
    let bins = fake_bins(&home, FAKE_CLI);
    let r = web(
        &mut app,
        json!({"op": "bins", "tailscaled": bins.tailscaled_bin, "tailscale": bins.tailscale_bin}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let r = web(
        &mut app,
        json!({"op": "save", "name": "exit", "kind": "tailscale", "exit_node": "exit-node-1"}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["view"]["routes"][0]["state"], "stopped");
    assert!(
        app.routes.ts.is_empty(),
        "nothing runs until a tab uses the route"
    );
    let cfg = crate::config::Config::parse(
        &std::fs::read_to_string(crate::config::Config::path()).unwrap(),
    )
    .unwrap();
    assert_eq!(cfg.routes.list[0], ts_route("exit-node-1"));
    assert_eq!(cfg.routes.tailscaled_bin, bins.tailscaled_bin);

    let dir = home.join("tailscale/exit");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("fake-status.json"),
        json!({"BackendState": "NeedsLogin", "AuthURL": "https://login.example.test/a/xyz789"})
            .to_string(),
    )
    .unwrap();

    let uid = app.panes[0].tabs[0].uid;
    web(
        &mut app,
        json!({"op": "set", "tab": tab_id(uid), "route": "exit"}),
    );
    app.launch(0, LaunchKind::Normal);
    assert!(
        !app.panes[0].tabs[0].is_running(),
        "held until the route is ready"
    );
    assert!(app.panes[0].tabs[0]
        .route_block
        .as_ref()
        .is_some_and(|b| b.auto));
    assert!(app.routes.ts.contains_key("exit"));
    wait_for("the login link is known", || {
        matches!(app.routes.ts["exit"].snapshot().state, TsState::Login(_))
    });
    let view = web(&mut app, json!({"op": "view"}));
    assert_eq!(view["result"]["routes"][0]["state"], "login");
    assert_eq!(
        view["result"]["routes"][0]["login_url"],
        "https://login.example.test/a/xyz789"
    );
    let agent = app
        .control_call("get_state", &json!({"_client": "brain"}))
        .to_string();
    assert!(
        !agent.contains("login.example.test"),
        "the login link is only for the user"
    );
    let pid = app.routes.ts["exit"]
        .snapshot()
        .pid
        .expect("tailscaled runs");
    assert!(crate::platform::alive(pid));

    web(
        &mut app,
        json!({"op": "set", "tab": tab_id(uid), "route": ""}),
    );
    app.routes.last_tick = None;
    app.routes_tick();
    assert!(app.routes.ts.is_empty(), "no tab uses it any more");
    wait_for("tailscaled was stopped", || !crate::platform::alive(pid));
    for l in lines(&home.join("bin/cli.log")) {
        assert!(l.starts_with(&format!("--socket={}", dir.display())), "{l}");
    }
    kill_all(&mut app, &home);
}

#[test]
#[ignore]
fn live_the_test_button_handshakes_with_the_api_host_through_a_proxy() {
    let (l, port) = listener();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { break };
            std::thread::spawn(move || {
                let mut head = vec![];
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if c.read(&mut b).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(b[0]);
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let target = text.split_whitespace().nth(1).unwrap_or("").to_string();
                let Ok(up) = TcpStream::connect(target.as_str()) else {
                    return;
                };
                c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .unwrap();
                let (mut c2, mut up2) = (c.try_clone().unwrap(), up.try_clone().unwrap());
                std::thread::spawn(move || std::io::copy(&mut c2, &mut up2));
                let (mut c, mut up) = (c, up);
                let _ = std::io::copy(&mut up, &mut c);
            });
        }
    });
    let u = ProxyUrl::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    let p = routes::probe(&u, routes::TEST_HOST);
    assert!(p.ok, "{p:?}");
    assert!(p.detail.contains("TLS handshake only"), "{p:?}");
}
