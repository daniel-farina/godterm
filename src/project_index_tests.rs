//! The project index on a fixture tree: a monorepo with workspaces and
//! several subprojects, a plain repo, a folder named like a subproject,
//! and sessions that worked in them.

use super::*;
use crate::config::testing::LOCK as ENV_LOCK;
use serde_json::json;

fn w(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// base/code/{platform, botmesh, billing}: platform has npm and pnpm
/// workspaces, turbo, apps/billing-dashboard, services/webhooks (Go,
/// README), packages/ui, plus things that must be skipped.
fn fixture(tag: &str) -> PathBuf {
    fixture_in(&std::env::temp_dir(), tag)
}

/// Sessions in a temp folder count as scripted runs (hidden), so the
/// app tests keep their tree in the build folder instead.
fn app_fixture(tag: &str) -> PathBuf {
    let t = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-fixtures");
    fixture_in(&t, tag)
}

fn fixture_in(at: &Path, tag: &str) -> PathBuf {
    let base = at.join(format!("godterm-pix-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let pf = base.join("code/platform");
    w(
        &pf.join("package.json"),
        r#"{"name": "platform", "private": true, "workspaces": ["apps/*", "packages/*"]}"#,
    );
    w(
        &pf.join("pnpm-workspace.yaml"),
        "packages:\n  - 'services/*'\n",
    );
    w(&pf.join("turbo.json"), "{}");
    w(
        &pf.join(".git/config"),
        "[remote \"origin\"]\n\turl = https://user:secret@github.com/acme/platform-mono.git\n",
    );
    w(
        &pf.join("apps/billing-dashboard/package.json"),
        r#"{"name": "@acme/billing-dashboard", "description": "Invoices and plans for customers"}"#,
    );
    w(
        &pf.join("apps/billing-dashboard/src/Invoice.tsx"),
        "export const Invoice = () => null;\n",
    );
    w(
        &pf.join("services/webhooks/go.mod"),
        "module github.com/acme/webhooks\n\ngo 1.22\n",
    );
    w(
        &pf.join("services/webhooks/README.md"),
        "# Webhook delivery with retries\n\nSends events.\n",
    );
    w(
        &pf.join("services/webhooks/retry/backoff.go"),
        "package retry\n// exponential backoff for failed deliveries\n",
    );
    w(
        &pf.join("packages/ui/package.json"),
        r#"{"name": "@acme/ui", "description": "Shared UI kit"}"#,
    );
    // Never read: dependencies, dot folders, secrets.
    w(
        &pf.join("node_modules/billing-leak/package.json"),
        r#"{"name": "billing-leak", "description": "should never be indexed"}"#,
    );
    w(
        &pf.join(".hidden/leakage/package.json"),
        r#"{"name": "leakage"}"#,
    );
    w(&pf.join("credentials.json"), r#"{"name": "leaky creds"}"#);
    w(&pf.join(".env"), "LEAK_TOKEN=abc\n");
    // A plain repo, and a folder named exactly "billing".
    w(
        &base.join("code/botmesh/Cargo.toml"),
        "[package]\nname = \"botmesh\"\ndescription = \"Bot generator\"\n",
    );
    w(&base.join("code/billing/README.md"), "# Billing\n");
    base
}

fn index_of(roots: &[PathBuf]) -> (Shared, Stats) {
    let sh: Shared = Default::default();
    refresh(&sh, roots, &Limits::default(), true, &[]);
    let st = sh.lock().unwrap().last.clone();
    (sh, st)
}

fn roots(base: &Path) -> Vec<PathBuf> {
    ["platform", "botmesh", "billing"]
        .iter()
        .map(|n| base.join("code").join(n))
        .collect()
}

fn top(sh: &Shared, q: &str) -> Hit {
    let ix = sh.lock().unwrap();
    let h = search(&ix, q, 10);
    assert!(!h.is_empty(), "{q}: no hit");
    h[0].clone()
}

#[test]
fn spoken_queries_find_the_subproject_with_reason_and_path() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base = fixture("spoken");
    let pf = base.join("code/platform");
    let (sh, st) = index_of(&roots(&base));
    assert_eq!(st.scanned, 3);
    {
        let ix = sh.lock().unwrap();
        let m = &ix.roots[&pf];
        assert_eq!(m.remote.as_deref(), Some("platform-mono"), "repo name only");
        assert!(
            m.workspaces.iter().any(|w| w == "turbo")
                && m.workspaces.iter().any(|w| w.contains("pnpm")),
            "{:?}",
            m.workspaces
        );
        let svc = m
            .entries
            .iter()
            .find(|e| e.rel == "services/webhooks")
            .unwrap();
        assert!(svc.member, "pnpm member");
        assert_eq!(svc.desc.as_deref(), Some("Webhook delivery with retries"));
    }
    let h = top(&sh, "the billing dashboard");
    assert_eq!(h.path, pf.join("apps/billing-dashboard"));
    assert!(
        h.why.contains("apps/billing-dashboard in") && h.why.contains("platform"),
        "{}",
        h.why
    );
    for q in ["web hooks", "the webhook retry thing", "webhooks service"] {
        assert_eq!(top(&sh, q).path, pf.join("services/webhooks"), "{q}");
    }
    let h = top(&sh, "bot mesh");
    assert_eq!(h.path, base.join("code/botmesh"));
    assert!(h.why.starts_with("project "), "{}", h.why);
    let h = top(&sh, "invoices");
    assert_eq!(h.path, pf.join("apps/billing-dashboard"));
    assert!(h.why.contains("Invoices and plans"), "{}", h.why);
    assert_eq!(top(&sh, "shared ui kit").path, pf.join("packages/ui"));
    let h = top(&sh, "the mono repo");
    assert_eq!(h.path, pf);
    assert!(h.why.contains("git remote"), "{}", h.why);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_direct_name_ranks_above_a_deep_match() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base = fixture("rank");
    let (sh, _) = index_of(&roots(&base));
    let ix = sh.lock().unwrap();
    let h = search(&ix, "billing", 10);
    assert_eq!(h[0].path, base.join("code/billing"), "{h:?}");
    let deep = h
        .iter()
        .position(|x| x.rel == "apps/billing-dashboard")
        .expect("the subproject is found too");
    assert!(deep > 0 && h[0].score > h[deep].score);
    // A subproject's name above a folder below it.
    let h = search(&ix, "webhooks", 10);
    assert_eq!(h[0].rel, "services/webhooks", "{h:?}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ignored_dirs_dotfiles_secrets_and_private_roots_are_skipped() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = std::env::temp_dir().join(format!("godterm-pix-home-{}", std::process::id()));
    crate::config::testing::set_home(&home);
    let base = fixture("skip");
    let private = crate::config::app_home().join("secret-proj");
    w(&private.join("package.json"), r#"{"name": "privateproj"}"#);
    let mut rs = roots(&base);
    rs.push(private.clone());
    let (sh, _) = index_of(&rs);
    let ix = sh.lock().unwrap();
    assert!(!ix.roots.contains_key(&private), "private root mapped");
    let m = &ix.roots[&base.join("code/platform")];
    for e in &m.entries {
        assert!(
            !e.rel.contains("node_modules")
                && !e.rel.contains(".hidden")
                && !e.rel.contains(".git"),
            "{}",
            e.rel
        );
    }
    assert!(m
        .watched
        .iter()
        .all(|(p, _, _)| !p.ends_with("credentials.json") && !p.ends_with(".env")));
    for q in ["leak", "leakage", "leaky creds", "privateproj"] {
        assert!(search(&ix, q, 10).is_empty(), "{q}");
    }
    // The deep search skips them too, names and contents.
    let r = deep_search(
        &[base.join("code/platform")],
        "leak",
        true,
        &DeepLimits::default(),
        &[],
    );
    assert!(r.hits.is_empty(), "{:?}", r.hits);
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn caps_and_time_budgets_hold() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base = fixture("caps");
    let wide = base.join("code/wide");
    for i in 0..60 {
        std::fs::create_dir_all(wide.join(format!("d{i:02}/inner"))).unwrap();
    }
    let lim = Limits {
        max_dirs: 10,
        ..Default::default()
    };
    let m = scan_root(&wide, &lim, &[]);
    assert!(m.truncated && m.entries.len() <= 10, "{}", m.entries.len());
    let zero = Limits {
        per_root: Duration::ZERO,
        ..Default::default()
    };
    assert!(scan_root(&wide, &zero, &[]).truncated);
    // The whole refresh stops at its budget: the rest waits.
    let sh: Shared = Default::default();
    let none = Limits {
        total: Duration::ZERO,
        ..Default::default()
    };
    refresh(&sh, &roots(&base), &none, true, &[]);
    assert_eq!(sh.lock().unwrap().last.pending, 3);
    // Deep search: entry cap, time cap, file size cap.
    let few = DeepLimits {
        max_entries: 5,
        ..Default::default()
    };
    let r = deep_search(std::slice::from_ref(&wide), "d59", false, &few, &[]);
    assert!(r.truncated && r.seen <= 5);
    let fast = DeepLimits {
        names_time: Duration::ZERO,
        ..Default::default()
    };
    assert!(deep_search(std::slice::from_ref(&wide), "inner", false, &fast, &[]).timed_out);
    let pf = base.join("code/platform");
    let r = deep_search(
        std::slice::from_ref(&pf),
        "exponential backoff",
        true,
        &DeepLimits::default(),
        &[],
    );
    assert!(
        r.hits
            .iter()
            .any(|h| h.kind == "content" && h.path.ends_with("backoff.go")),
        "{:?}",
        r.hits
    );
    let tiny = DeepLimits {
        max_file: 8,
        ..Default::default()
    };
    let r = deep_search(&[pf], "exponential backoff", true, &tiny, &[]);
    assert!(r.hits.iter().all(|h| h.kind != "content"), "{:?}", r.hits);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_change_rescans_only_that_root() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base = fixture("incr");
    let rs = roots(&base);
    let (sh, st) = index_of(&rs);
    assert_eq!((st.scanned, st.reused), (3, 0));
    refresh(&sh, &rs, &Limits::default(), true, &[]);
    let st = sh.lock().unwrap().last.clone();
    assert_eq!((st.scanned, st.reused), (0, 3), "nothing changed");
    // Not due yet: not even checked.
    refresh(&sh, &rs, &Limits::default(), false, &[]);
    assert_eq!(sh.lock().unwrap().last.scanned, 0);
    w(
        &base.join("code/platform/apps/payments-portal/package.json"),
        r#"{"name": "payments-portal"}"#,
    );
    refresh(&sh, &rs, &Limits::default(), true, &[]);
    let st = sh.lock().unwrap().last.clone();
    assert_eq!((st.scanned, st.reused), (1, 2), "only platform again");
    assert_eq!(
        top(&sh, "payments portal").path,
        base.join("code/platform/apps/payments-portal")
    );
    // A manifest edited in place counts too.
    std::thread::sleep(Duration::from_millis(20));
    w(
        &base.join("code/botmesh/Cargo.toml"),
        "[package]\nname = \"botmesh\"\ndescription = \"Swarm simulator\"\n",
    );
    refresh(&sh, &rs, &Limits::default(), true, &[]);
    assert_eq!(sh.lock().unwrap().last.scanned, 1);
    assert_eq!(top(&sh, "swarm simulator").path, base.join("code/botmesh"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn tokens_split_spoken_and_written_forms() {
    assert_eq!(tokens("BillingDashboard"), vec!["billing", "dashboard"]);
    assert_eq!(
        tokens("@acme/billing-dashboard"),
        vec!["acme", "billing", "dashboard"]
    );
    assert_eq!(
        query_tokens("the webhook retry thing"),
        vec!["webhook", "retry"]
    );
    assert!(word_match("webhook", "webhooks") > 0.9);
    assert_eq!(word_match("bill", "dashboard"), 0.0);
}

#[test]
fn grok_rename_sets_its_manual_title() {
    let base = std::env::temp_dir().join(format!("godterm-pix-grok-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("sessions/%2Fx%2Fweb/0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee");
    w(
        &dir.join("summary.json"),
        &json!({"info": {"id": "0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee", "cwd": "/x/web"}, "num_chat_messages": 4, "generated_title": "Clock page", "session_kind": null}).to_string(),
    );
    let before = crate::harness::grok::parse_session_any(&dir, true).unwrap();
    assert_eq!(before.name, None, "a generated title is not a name");
    crate::session_rename::set_grok_title(&dir, "Billing retries").unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).unwrap()).unwrap();
    assert_eq!(v["title_is_manual"], json!(true));
    assert_eq!(v["num_chat_messages"], json!(4), "the rest is kept");
    let after = crate::harness::grok::parse_session_any(&dir, true).unwrap();
    assert_eq!(after.name.as_deref(), Some("Billing retries"));
    assert_eq!(after.summary(), "Billing retries");
    let _ = std::fs::remove_dir_all(&base);
}

// ---- through the assistant's tools ----

fn test_app(tag: &str) -> (crate::app::App, PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-pix-app-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    (app, home)
}

const SID: &str = "5555eeee-0000-4000-8000-000000000005";
const SID2: &str = "6666ffff-0000-4000-8000-000000000006";

/// A Claude session in the monorepo that edited the billing dashboard.
fn session_fixture(app: &crate::app::App, pf: &Path) -> PathBuf {
    let sp = app.cfg.accounts[0]
        .config_dir()
        .join("projects")
        .join("-x-platform");
    let cwd = pf.to_string_lossy();
    let file = format!("{cwd}/apps/billing-dashboard/src/Invoice.tsx");
    let lines = [
        json!({"type": "user", "cwd": cwd, "sessionId": SID, "message": {"role": "user", "content": "fix the totals"}}),
        json!({"type": "assistant", "cwd": cwd, "message": {"id": "m1", "role": "assistant", "model": "claude-x", "content": [{"type": "tool_use", "name": "Edit", "input": {"file_path": file}}]}}),
    ];
    let mut text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    w(&sp.join(format!("{SID}.jsonl")), &text);
    text = text
        .replace(SID, SID2)
        .replace("fix the totals", "tidy the readme");
    let text = text.replace("apps/billing-dashboard/src/Invoice.tsx", "README.md");
    w(&sp.join(format!("{SID2}.jsonl")), &text);
    sp.join(format!("{SID}.jsonl"))
}

fn hits(v: &Value) -> Vec<Value> {
    v["result"]["hits"].as_array().cloned().unwrap_or_default()
}

use serde_json::Value;

#[test]
fn find_and_sessions_look_inside_projects() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("find");
    let base = app_fixture("appfind");
    let pf = base.join("code/platform");
    session_fixture(&app, &pf);
    app.kick_index(true);
    assert!(
        app.project_index.lock().unwrap().roots.contains_key(&pf),
        "a session's folder is mapped"
    );
    let v = app.control_call("find", &json!({"query": "the billing dashboard"}));
    let h = hits(&v);
    assert_eq!(h[0]["type"], "project", "{v}");
    assert_eq!(
        h[0]["path"],
        json!(pf.join("apps/billing-dashboard").to_string_lossy())
    );
    assert!(
        h[0]["why"]
            .as_str()
            .unwrap()
            .contains("apps/billing-dashboard"),
        "{v}"
    );
    assert_eq!(v["result"]["strong"], json!(true));
    // The session that touched it, and why.
    let v = app.control_call("find", &json!({"query": "invoice"}));
    let s = hits(&v)
        .into_iter()
        .find(|h| h["type"] == "session")
        .unwrap_or_else(|| panic!("{v}"));
    assert_eq!(s["id"], SID);
    assert!(
        s["why"]
            .as_str()
            .unwrap()
            .starts_with("touched apps/billing-dashboard"),
        "{s}"
    );
    let v = app.control_call("sessions", &json!({"text": "billing dashboard"}));
    let ids: Vec<&str> = v["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    assert_eq!(ids, vec![SID], "{v}");
    // No strong hit: told to look deeper; search_projects does.
    let v = app.control_call("find", &json!({"query": "exponential backoff"}));
    assert_eq!(v["result"]["strong"], json!(false), "{v}");
    assert!(v["result"]["next"]
        .as_str()
        .unwrap()
        .contains("search_projects"));
    let v = app.control_call(
        "search_projects",
        &json!({"query": "exponential backoff", "content": true}),
    );
    assert!(
        hits(&v)
            .iter()
            .any(|h| h["path"].as_str().unwrap().ends_with("retry/backoff.go")),
        "{v}"
    );
    // The subproject path is something open_path takes.
    let v = app.control_call(
        "open_path",
        &json!({"paths": [pf.join("services/webhooks").to_string_lossy()]}),
    );
    assert_eq!(
        v["result"]["opened"].as_array().map(|a| a.len()),
        Some(1),
        "{v}"
    );
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn saved_descriptions_come_first_and_go_stale() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("memo");
    let base = app_fixture("memo");
    let pf = base.join("code/platform");
    session_fixture(&app, &pf);
    app.kick_index(true);
    let hooks = pf.join("services/webhooks");
    let v = app.control_call(
        "describe_project",
        &json!({"path": hooks.to_string_lossy(), "description": "Delivers customer webhooks with retries", "aliases": ["hook relay", "the pinger"], "tags": ["go"]}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    // Spoken forms of an alias.
    for q in ["hook relay", "the hook re lay", "pinger"] {
        let v = app.control_call("find", &json!({"query": q}));
        let h = hits(&v);
        assert!(!h.is_empty(), "{q}: {v}");
        assert_eq!(h[0]["type"], "project", "{q}: {v}");
        assert_eq!(
            h[0]["path"],
            json!(hooks.canonicalize().unwrap().to_string_lossy()),
            "{q}"
        );
        assert!(
            h[0]["why"]
                .as_str()
                .unwrap()
                .starts_with("from saved description"),
            "{v}"
        );
        assert!(h[0]["saved"]["stale"].is_null(), "{v}");
    }
    // A session description shows in sessions and find.
    let v = app.control_call(
        "describe_session",
        &json!({"id": &SID[..8], "description": "Totals bug in the invoices page", "aliases": ["totals fix"]}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    let v = app.control_call("find", &json!({"query": "totals fix"}));
    assert_eq!(hits(&v)[0]["id"], SID, "{v}");
    let v = app.control_call("sessions", &json!({"text": "invoices page"}));
    assert_eq!(
        v["result"]["sessions"][0]["saved"]["description"], "Totals bug in the invoices page",
        "{v}"
    );
    // Old: marked stale. A deep search on its root refreshes the date.
    let mut store = crate::project_memory::load();
    let old = (chrono::Local::now() - chrono::Duration::days(30))
        .format("%Y-%m-%d")
        .to_string();
    for m in store.items.iter_mut() {
        m.last_deep_search = Some(old.clone());
    }
    crate::project_memory::save(&store).unwrap();
    let v = app.control_call("find", &json!({"query": "hook relay"}));
    assert_eq!(hits(&v)[0]["saved"]["stale"], json!(true), "{v}");
    assert!(hits(&v)[0]["saved"]["stale_reason"]
        .as_str()
        .unwrap()
        .contains("days ago"));
    app.control_call(
        "search_projects",
        &json!({"query": "retry", "path": pf.to_string_lossy()}),
    );
    let store = crate::project_memory::load();
    let today = crate::project_memory::today();
    assert_eq!(
        store.deep_searched.get(&*pf.to_string_lossy()),
        Some(&today)
    );
    let m = store
        .get("project", &hooks.canonicalize().unwrap().to_string_lossy())
        .unwrap();
    assert_eq!(m.last_deep_search.as_deref(), Some(today.as_str()));
    let v = app.control_call("find", &json!({"query": "hook relay"}));
    assert!(hits(&v)[0]["saved"]["stale"].is_null(), "{v}");
    assert!(hits(&v)[0]["why"]
        .as_str()
        .unwrap()
        .contains(&format!("deep-searched {today}")));
    // The tree changed a lot since it was saved: stale again.
    for i in 0..12 {
        std::fs::create_dir_all(hooks.join(format!("handlers/h{i}"))).unwrap();
    }
    app.kick_index(true);
    let v = app.control_call("find", &json!({"query": "hook relay"}));
    assert!(
        hits(&v)[0]["saved"]["stale_reason"]
            .as_str()
            .unwrap_or("")
            .contains("changed a lot"),
        "{v}"
    );
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn rename_session_offline_live_and_batch() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("rename");
    let base = app_fixture("rename");
    let pf = base.join("code/platform");
    let file = session_fixture(&app, &pf);
    app.kick_index(true);
    // Offline: the title records are appended, the index sees the name.
    let v = app.control_call(
        "rename_session",
        &json!({"id": &SID[..8], "name": "Billing retries"}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    let text = std::fs::read_to_string(&file).unwrap();
    let last: Vec<Value> = text
        .lines()
        .rev()
        .take(2)
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(last[1]["type"], "custom-title");
    assert_eq!(last[1]["customTitle"], "Billing retries");
    assert_eq!(last[0]["type"], "agent-name");
    {
        let ix = app.session_index.lock().unwrap();
        let r = ix.rows.iter().find(|r| r.info.id == SID).unwrap();
        assert_eq!(r.info.name.as_deref(), Some("Billing retries"));
    }
    let v = app.control_call("find", &json!({"query": "billing retries"}));
    assert!(hits(&v).iter().any(|h| h["id"] == SID), "{v}");
    // Live in a tab: /rename goes to claude there, the tab is renamed.
    let t = app.panes[0].add_tab(pf.clone());
    app.panes[0].tabs[t].session_id = Some(SID2.into());
    app.panes[0].tabs[t].pending = Some(crate::pane::LaunchKind::Resume(SID2.into()));
    let before = std::fs::read_to_string(file.with_file_name(format!("{SID2}.jsonl"))).unwrap();
    let v = app.control_call(
        "rename_session",
        &json!({"id": SID2, "name": "Readme pass"}),
    );
    assert_eq!(
        v["result"]["renamed"][0]["how"], "/rename in its tab",
        "{v}"
    );
    let uid = app.panes[0].tabs[t].uid;
    assert!(
        app.deliveries
            .iter()
            .any(|d| d.uid == uid && d.text == "/rename Readme pass"),
        "delivered"
    );
    assert_eq!(
        app.panes[0].tabs[t].custom_name.as_deref(),
        Some("Readme pass")
    );
    let after = std::fs::read_to_string(file.with_file_name(format!("{SID2}.jsonl"))).unwrap();
    assert_eq!(before, after, "the live file is left to claude");
    // Several at once ask first.
    let v = app.control_call(
        "rename_session",
        &json!({"id": [SID, SID2], "name": ["One", "Two"]}),
    );
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&home);
}
