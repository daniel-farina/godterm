//! Sync tests on fixture homes with local bare repos standing in for
//! GitHub. Nothing here reads the user's real homes or reaches GitHub.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

/// A fixture root outside the temp folders (sessions there are left out
/// on purpose) and outside every real user folder.
pub(super) fn root(name: &str) -> PathBuf {
    let r = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/tmp/sync-tests")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&r);
    std::fs::create_dir_all(&r).unwrap();
    assert!(!crate::config::is_real_user_dir(&r));
    r
}

pub(super) fn w(p: &Path, data: impl AsRef<[u8]>) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

pub(super) fn sh_git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@x",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
}

const SENTINEL: &str = "FORBIDDEN-SENTINEL-7f3a";

pub(super) fn config_text(remotes: &Path, accounts: bool) -> String {
    // Machine A's, or (no accounts) a fresh machine B with its own local
    // keys and other portable values.
    let (bin, mode, speed, mic) = if accounts {
        ("/opt/local/claude", "auto", "1.5", "Fixture Mic")
    } else {
        ("/usr/b/claude", "default", "1.0", "B Mic")
    };
    let mut s = format!(
        "claude_bin = \"{bin}\"\nremember_window = false\npermission_mode = \"{mode}\"\n\n[voice]\ntts_speed = {speed}\ndevice = \"{mic}\"\n\n[sync]\nenabled = false\nowner = \"tester\"\naccount = \"tester\"\nremote = \"{}\"\n",
        super::git::file_url(remotes)
    );
    if accounts {
        s += "\n[[account]]\nname = \"work\"\nlabel = \"Work\"\n\n[[account]]\nname = \"g\"\nlabel = \"Grok\"\nharness = \"grok\"\n";
    }
    s
}

pub(super) fn ctx_for(home: &Path, cfg: &str) -> Ctx {
    let app = home.join(".godterm");
    w(&app.join("config.toml"), cfg);
    let c = crate::config::Config::parse(cfg).unwrap();
    let mut x = Ctx::new(home.to_path_buf(), app, c);
    // The fixture's own vault stand-in, besides /Volumes/Private.
    x.never.push(paths::real(&home.join("vault")));
    x
}

pub(super) fn line(sid: &str, cwd: &Path, text: &str) -> String {
    json!({"type": "user", "sessionId": sid, "cwd": cwd, "message": {"role": "user", "content": text}}).to_string() + "\n"
}

/// Machine A: accounts, stores, sessions, a project with .env files, a
/// bound repo, and every file the sync must never read (each holding the
/// sentinel).
pub(super) fn fixture_a(r: &Path) -> (Ctx, PathBuf) {
    let home = r.join("a");
    let remotes = r.join("remotes");
    std::fs::create_dir_all(&remotes).unwrap();
    let ctx = ctx_for(&home, &config_text(&remotes, true));
    let app = &ctx.app_home;
    let api = home.join("code").join("api");
    // The project: a repo with a GitHub-like remote (a bare repo here).
    let bare = remotes.join("tester/api.git");
    std::fs::create_dir_all(&bare).unwrap();
    sh_git(&bare, &["init", "-q", "--bare"]);
    std::fs::create_dir_all(&api).unwrap();
    sh_git(&api, &["init", "-q"]);
    w(&api.join(".gitignore"), ".env\n");
    w(&api.join("main.rs"), "fn main() {}\n");
    sh_git(&api, &["add", "-A"]);
    sh_git(&api, &["commit", "-q", "-m", "first"]);
    sh_git(
        &api,
        &["remote", "add", "origin", &super::git::file_url(&bare)],
    );
    sh_git(
        &api,
        &["push", "-q", "-u", "origin", "HEAD:refs/heads/main"],
    );
    w(&api.join(".env"), "API_KEY=abc\nDB=1\n");
    w(&api.join(".env.local"), "LOCAL=1\n");
    w(&api.join(".env.example"), "API_KEY=\n");
    // A project with no repo at all: kept, with a warning.
    std::fs::create_dir_all(home.join("code/notes")).unwrap();
    w(&home.join("code/notes/.env"), "N=1\n");
    let acct = app.join("accounts/work");
    let enc = crate::state::encode_project_dir(&api);
    let pdir = acct.join("projects").join(&enc);
    w(&pdir.join("S1.jsonl"), line("S1", &api, "fix the build"));
    w(
        &pdir.join("S2.jsonl"),
        line("S2", &api, "write docs") + &line("S2", &api, "more"),
    );
    w(
        &pdir.join("V2.jsonl"),
        line("V2", &api, "read /Volumes/Private/notes.md"),
    );
    w(&pdir.join("memory/MEMORY.md"), "- remember this\n");
    let vdir = acct.join("projects/-Volumes-Private-x");
    w(
        &vdir.join("V1.jsonl"),
        line("V1", Path::new("/Volumes/Private/x"), "hi"),
    );
    let tdir = acct.join("projects/-private-tmp-foo");
    w(
        &tdir.join("T1.jsonl"),
        line("T1", Path::new("/private/tmp/foo"), "hi"),
    );
    w(&acct.join("settings.json"), "{\"theme\":\"dark\"}");
    w(&acct.join("CLAUDE.md"), "be brief\n");
    w(&acct.join("skills/review/SKILL.md"), "review\n");
    w(&acct.join("commands/x.md"), "x\n");
    w(&acct.join("agents/y.md"), "y\n");
    w(
        &acct.join("history.jsonl"),
        "{\"display\":\"a\",\"timestamp\":1}\n",
    );
    let gacct = app.join("accounts/g");
    let gs = gacct
        .join("sessions")
        .join(crate::demo_agent::grok_dir_name(&api))
        .join("G1");
    w(
        &gs.join("summary.json"),
        "{\"generated_title\":\"grok one\",\"info\":{\"id\":\"G1\"}}",
    );
    w(&gs.join("messages.jsonl"), "{\"m\":1}\n");
    // GodTerm's own files.
    w(
        &app.join("state.json"),
        json!({"slots": [{"account": "work", "tabs": [{"cwd": api, "session_id": "S1", "uid": 7, "name": "api"}]}],
               "closed_tabs": [{"account": "work", "cwd": home.join("vault/proj"), "session_id": "X", "loops": [], "group": 0, "closed_at": 1, "label": "v"}],
               "next_uid": 9})
        .to_string(),
    );
    w(
        &app.join("archive.json"),
        json!({"entries": [{"id": "a1", "provider": "claude", "account": "work", "cwd": api, "session_id": "S2", "label": "docs", "last_activity": 1, "archived_at": 2}]}).to_string(),
    );
    w(
        &app.join("jobs.json"),
        json!({"jobs": [{"id": "j1", "name": "nightly", "prompt": "check", "target": {"session_id": "S2", "provider": "claude", "account": "work", "cwd": api, "label": "docs"}, "created_by": "user", "enabled": true, "catch_up": "skip", "created_at": 1}]}).to_string(),
    );
    w(&app.join("projects.json"), json!({"version": 1, "items": [{"kind": "project", "key": api, "description": "the api"}, {"kind": "project", "key": home.join("code/notes"), "description": "notes"}]}).to_string());
    w(&app.join("assistant/learned.json"), json!({"rules": [{"id": 1, "text": "be kind", "reason": "", "source_turn": "", "created": "2026-01-01T00:00:00Z", "updated": "", "enabled": true, "scope": "global"}], "next_id": 2, "rev": 1}).to_string());
    w(
        &app.join("assistant/conversations/2026-10-25-120000-abc.jsonl"),
        "{\"kind\":\"user\"}\n",
    );
    w(
        &app.join("git-bindings.json"),
        json!({"version": 1, "projects": {"~/code/api": {"owner": "tester", "repo": "api", "account": "tester"}}}).to_string(),
    );
    // Never read: each holds the sentinel.
    for f in FORBIDDEN_APP {
        w(&app.join(f), SENTINEL);
    }
    for f in FORBIDDEN_ACCT {
        w(&acct.join(f), SENTINEL);
    }
    w(&gacct.join("auth.json"), SENTINEL);
    w(&home.join("vault/proj/.env"), SENTINEL);
    (ctx, home)
}

const FORBIDDEN_APP: &[&str] = &[
    "control.json",
    "control.sock",
    "web-token",
    "tab-mcp/t1.json",
    "godterm.lock",
    "godterm.lock.json",
    "godterm.log",
    "usage/x.json",
    "session-index.json",
    "project-index.json",
    "updates/x.json",
    "bin/godterm-speech",
    "voice/model.onnx",
    "trash/x/y.jsonl",
    "assistant/mcp.json",
    "web-client.log",
    "web-push-vapid.pk8",
    "web-push-subs.json",
    "config.toml.bak",
    "migration-backup/config.toml",
];

const FORBIDDEN_ACCT: &[&str] = &[
    ".credentials.json",
    ".claude.json",
    "shell-snapshots/snap.sh",
    "session-env/S1/env",
    "sessions/123.json",
    "statsig/x",
    "telemetry/x",
    "cache/x",
    "chrome/x",
    "paste-cache/x",
    "file-history/S1/x",
    "plugins/cache/x",
    "todos/x.json",
    "skills/credentials.json",
    "skills/token.json",
    "projects/x/S1/subagents/a.jsonl",
];

#[test]
fn fixture_home_walk_never_yields_a_forbidden_file() {
    let r = root("walk");
    let (ctx, home) = fixture_a(&r);
    let c = collect::collect(&ctx);
    for s in c.sources() {
        let t = std::fs::read_to_string(&s).unwrap_or_default();
        assert!(
            !t.contains(SENTINEL),
            "read a forbidden file: {}",
            s.display()
        );
        assert!(!s.starts_with(home.join("vault")));
    }
    let enc = crate::state::encode_project_dir(&home.join("code").join("api"));
    let mem = format!("memory/work/{}", portable::path_key("~/code/api"));
    let mut golden: Vec<String> = vec![
        "archive.json".into(),
        "assistant/conversations/2026-10-25-120000-abc.jsonl".into(),
        "claude/work/CLAUDE.md".into(),
        "claude/work/agents/y.md".into(),
        "claude/work/commands/x.md".into(),
        "claude/work/history.jsonl".into(),
        "claude/work/settings.json".into(),
        "claude/work/skills/review/SKILL.md".into(),
        "config/portable.toml".into(),
        "git/bindings.json".into(),
        "jobs.json".into(),
        "learned.json".into(),
        "machines/<id>/state.json".into(),
        format!("{mem}/MEMORY.md"),
        "projects.json".into(),
        "sessions/g/G1.json".into(),
        "sessions/work/S1.json".into(),
        "sessions/work/S2.json".into(),
        // Collected; left out at sync time for naming the vault.
        "sessions/work/V2.json".into(),
    ];
    golden.sort();
    assert_eq!(c.repo_paths(), golden);
    // Left out by folder: the vault and a temp folder.
    let why: Vec<&str> = c.skipped.iter().map(|(a, _)| a.as_str()).collect();
    assert!(
        why.contains(&"V1") && why.contains(&"T1"),
        "{:?}",
        c.skipped
    );
    let _ = enc;
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn plan_totals_and_vault_by_content() {
    let r = root("plan");
    let (ctx, _home) = fixture_a(&r);
    let p = plan::plan(&ctx);
    assert_eq!(p.sessions, 3, "S1, S2, G1: {p:?}");
    assert_eq!(p.vault_mentions, 1, "V2 names the vault");
    assert_eq!(p.hot, 2, "S1 in a tab, S2 archived and a job's");
    assert_eq!(p.env_files, 3, ".env and .env.local of api, .env of notes");
    assert_eq!(p.env_projects, 2);
    assert!(
        p.env_not_ignored
            .iter()
            .any(|e| e.ends_with("code/api/.env.local")),
        "{p:?}"
    );
    assert_eq!(p.stores["archive"], 1);
    assert_eq!(p.stores["jobs"], 1);
    let total: u64 = ["S1", "S2"]
        .iter()
        .map(|s| {
            std::fs::metadata(
                ctx.app_home
                    .join("accounts/work/projects")
                    .join(crate::state::encode_project_dir(
                        &ctx.home.join("code").join("api"),
                    ))
                    .join(format!("{s}.jsonl")),
            )
            .unwrap()
            .len()
        })
        .sum::<u64>()
        + "{\"generated_title\":\"grok one\",\"info\":{\"id\":\"G1\"}}".len() as u64
        + "{\"m\":1}\n".len() as u64;
    assert_eq!(p.session_bytes, total);
    assert_eq!(p.new_bytes, total, "nothing pushed yet");
    assert!(
        p.projects_not_backed_up
            .iter()
            .any(|l| l.contains("code/notes") && l.contains("no git repo")),
        "{p:?}"
    );
    assert!(p.text().contains("conversations: 3"));
    let _ = std::fs::remove_dir_all(&r);
}

/// Every blob in a bare repo, as text.
fn all_blobs(bare: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(bare)
        .args(["rev-list", "--objects", "--all"])
        .output()
        .unwrap();
    let mut s = String::new();
    for l in String::from_utf8_lossy(&out.stdout).lines() {
        let id = l.split_whitespace().next().unwrap_or("");
        let o = Command::new("git")
            .arg("-C")
            .arg(bare)
            .args(["cat-file", "-p", id])
            .output()
            .unwrap();
        s.push_str(&String::from_utf8_lossy(&o.stdout));
    }
    s
}

pub(super) fn ls(bare: &Path, rev: &str) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(bare)
        .args(["ls-tree", "-r", "--name-only", rev])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

pub(super) fn session_file(ctx: &Ctx, acct: &str, cwd: &Path, id: &str) -> PathBuf {
    ctx.app_home
        .join("accounts")
        .join(acct)
        .join("projects")
        .join(crate::state::encode_project_dir(cwd))
        .join(format!("{id}.jsonl"))
}

#[test]
fn end_to_end_two_homes_init_then_clone() {
    let r = root("e2e");
    let (a, home_a) = fixture_a(&r);
    let remotes = r.join("remotes");
    // First machine: no question for a local stand-in.
    let rep = restore::init(&a, a.cfg.sync.clone(), &|_| {
        panic!("no yes is asked for a local remote")
    })
    .unwrap();
    assert!(rep.ok, "{rep:?}");
    assert!(rep.pushed && rep.sessions_pushed == 3, "{rep:?}");
    let a = restore::reload(&a).unwrap();
    assert!(a.sync().enabled);
    let state_bare = remotes.join("tester/godterm-state.git");
    let month = shards::this_month();
    let shard_bare = remotes.join(format!("tester/godterm-convos-{month}.git"));
    assert!(shard_bare.is_dir());
    let files = ls(&state_bare, "main");
    for f in [
        "format.json",
        "README.md",
        ".gitattributes",
        "config/portable.toml",
        "accounts.json",
        "sessions/work/S1.json",
        "sessions/g/G1.json",
        "jobs.json",
        "shards.json",
    ] {
        assert!(files.iter().any(|x| x == f), "{f} missing: {files:?}");
    }
    assert!(
        !files
            .iter()
            .any(|f| f.contains("V1") || f.contains("V2") || f.contains("T1")),
        "{files:?}"
    );
    assert!(files
        .iter()
        .any(|f| f.starts_with("env/") && f.ends_with("/.env.local")));
    assert!(!files.iter().any(|f| f.ends_with(".env.example")));
    let leases = ls(&state_bare, "leases");
    assert!(leases.iter().any(|f| f == "leases/.keep"));
    assert!(leases
        .iter()
        .any(|f| f == &format!("heartbeats/{}.json", a.machine.id)));
    // Nothing forbidden in any object of either repo, and no machine
    // local config key.
    let blobs = all_blobs(&state_bare) + &all_blobs(&shard_bare);
    assert!(!blobs.contains(SENTINEL));
    assert!(!blobs.contains("/opt/local/claude") && !blobs.contains("Fixture Mic"));
    assert!(blobs.contains("tts_speed = 1.5"));
    assert!(!blobs.contains("/Volumes/Private/notes"));

    // Second machine: another home path, no accounts yet.
    let home_b = r.join("b");
    let b = ctx_for(&home_b, &config_text(&remotes, false));
    let o = restore::CloneOpts {
        label: Some("Linux".into()),
        tabs_from: Some("auto".into()),
        ..Default::default()
    };
    let cl = restore::clone(&b, "tester/godterm-state", &o).unwrap();
    assert!(cl.report.ok, "{:?}", cl.report);
    let b = restore::reload(&b).unwrap();
    assert_ne!(b.machine.id, a.machine.id, "a machine id is never copied");
    assert_eq!(b.machine.label, "Linux");
    // Config: portable keys came over, local ones did not.
    let cfg_b = std::fs::read_to_string(b.app_home.join("config.toml")).unwrap();
    assert!(
        cfg_b.contains("tts_speed = 1.5") && cfg_b.contains("permission_mode = \"auto\""),
        "{cfg_b}"
    );
    assert!(!cfg_b.contains("/opt/local/claude") && !cfg_b.contains("Fixture Mic"));
    assert!(
        cfg_b.contains("/usr/b/claude") && cfg_b.contains("B Mic"),
        "local keys stay"
    );
    assert_eq!(
        b.cfg
            .accounts
            .iter()
            .map(|x| x.name.as_str())
            .collect::<Vec<_>>(),
        vec!["work", "g"]
    );
    // The project was cloned to its mapped folder; its .env files are back
    // as 0600.
    let api_b = home_b.join("code").join("api");
    assert!(api_b.join("main.rs").is_file(), "{:?}", cl.projects);
    assert_eq!(
        std::fs::read_to_string(api_b.join(".env")).unwrap_or_default(),
        "API_KEY=abc\nDB=1\n",
        "{:?} {:?}",
        cl.projects,
        cl.report
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(api_b.join(".env"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    // The tab's session resumes from the mapped path, byte for byte.
    let s1_a = session_file(&a, "work", &home_a.join("code").join("api"), "S1");
    let s1_b = session_file(&b, "work", &api_b, "S1");
    assert_eq!(std::fs::read(&s1_b).unwrap(), std::fs::read(&s1_a).unwrap());
    let st: Value =
        serde_json::from_slice(&std::fs::read(b.app_home.join("state.json")).unwrap()).unwrap();
    let tab = &st["slots"][0]["tabs"][0];
    assert_eq!(tab["cwd"], json!(api_b));
    assert_eq!(tab["session_id"], "S1");
    assert!(tab.get("uid").is_none(), "fresh tab ids here");
    assert_eq!(cl.tabs, 1);
    // Memory, skills, history and the grok session too.
    assert!(b
        .app_home
        .join("accounts/work/projects")
        .join(crate::state::encode_project_dir(&api_b))
        .join("memory/MEMORY.md")
        .is_file());
    assert!(b
        .app_home
        .join("accounts/work/skills/review/SKILL.md")
        .is_file());
    assert!(b
        .app_home
        .join("accounts/g/sessions")
        .join(crate::demo_agent::grok_dir_name(&api_b))
        .join("G1/messages.jsonl")
        .is_file());
    // A job from another machine waits for a yes here.
    let jobs: Value =
        serde_json::from_slice(&std::fs::read(b.app_home.join("jobs.json")).unwrap()).unwrap();
    assert_eq!(jobs["jobs"][0]["enabled"], false);
    assert_eq!(jobs["jobs"][0]["target"]["cwd"], json!(api_b));
    // ... and stays on on machine A after B syncs again.
    let rb = state_repo::sync_now(&b).unwrap();
    assert!(rb.ok, "{rb:?}");
    let ra = state_repo::sync_now(&a).unwrap();
    assert!(ra.ok, "{ra:?}");
    let jobs_a: Value =
        serde_json::from_slice(&std::fs::read(a.app_home.join("jobs.json")).unwrap()).unwrap();
    assert_eq!(jobs_a["jobs"][0]["enabled"], true);

    // A writes more of S2; B (not in a tab there) is extended.
    let s2_a = session_file(&a, "work", &home_a.join("code").join("api"), "S2");
    let s2_b = session_file(&b, "work", &api_b, "S2");
    assert!(s2_b.is_file(), "S2 is in the hot set: {:?}", cl.placed);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&s2_a)
        .unwrap();
    std::io::Write::write_all(
        &mut f,
        line("S2", &home_a.join("code").join("api"), "third").as_bytes(),
    )
    .unwrap();
    drop(f);
    assert!(state_repo::sync_now(&a).unwrap().ok);
    let rb = state_repo::sync_now(&b).unwrap();
    assert_eq!(rb.extended, 1, "{rb:?}");
    assert_eq!(std::fs::read(&s2_b).unwrap(), std::fs::read(&s2_a).unwrap());

    // Both write S2 differently: never merged. B's own lines become a fork.
    let add = |p: &Path, t: &str| {
        let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
        std::io::Write::write_all(&mut f, t.as_bytes()).unwrap();
    };
    add(
        &s2_a,
        &line("S2", &home_a.join("code").join("api"), "from A"),
    );
    add(&s2_b, &line("S2", &api_b, "from B"));
    assert!(state_repo::sync_now(&a).unwrap().ok);
    let rb = state_repo::sync_now(&b).unwrap();
    assert_eq!(rb.forked.len(), 1, "{rb:?}");
    assert_eq!(
        std::fs::read(&s2_b).unwrap(),
        std::fs::read(&s2_a).unwrap(),
        "the original id has the remote version"
    );
    let fork_id = rb.forked[0].split(" -> ").nth(1).unwrap().to_string();
    let fork = state_repo::load_manifest(&b.state_dir(), "work", &fork_id).unwrap();
    assert_eq!(fork.forked_from.as_deref(), Some("S2"));
    let ft = std::fs::read_to_string(session_file(&b, "work", &api_b, &fork_id)).unwrap();
    assert!(ft.contains("from B") && ft.contains("(fork from Linux,"));
    // The remote shows the fork after A pulls; refs only ever moved
    // forward.
    assert!(state_repo::sync_now(&a).unwrap().ok);
    assert!(state_repo::load_manifest(&a.state_dir(), "work", &fork_id).is_some());

    // Audit every git call of this test: no force anywhere, and only
    // GodTerm's own sync repos were written (never a project repo).
    let mine: Vec<_> = git::record::all()
        .into_iter()
        .filter(|(d, _)| d.starts_with(&r))
        .collect();
    assert!(!mine.is_empty());
    for (dir, args) in &mine {
        assert!(git::check_args(args).is_ok(), "{args:?}");
        assert!(!args.iter().any(|x| x.contains("force")), "{args:?}");
        let sub = args
            .iter()
            .find(|x| !x.starts_with('-'))
            .cloned()
            .unwrap_or_default();
        if [
            "commit",
            "commit-tree",
            "push",
            "update-ref",
            "merge",
            "rebase",
        ]
        .contains(&sub.as_str())
        {
            assert!(git::is_sync_repo(dir), "git {sub} in {}", dir.display());
            assert!(
                dir.starts_with(home_a.join(".godterm/sync"))
                    || dir.starts_with(home_b.join(".godterm/sync")),
                "{}",
                dir.display()
            );
        }
    }
    // The runner refuses writes outside a sync repo outright.
    assert!(git::run(
        &home_a.join("code").join("api"),
        &["commit", "--allow-empty", "-m", "x"],
        None
    )
    .is_err());
    assert!(git::run(
        &home_a.join("code").join("api"),
        &["push", "origin", "HEAD:refs/heads/main"],
        None
    )
    .is_err());
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn history_lines_union_in_time_order() {
    let l = |t: i64, s: &str| {
        json!({"display": s, "timestamp": t})
            .to_string()
            .into_bytes()
    };
    let a = vec![l(1, "a"), l(3, "c")];
    let b = vec![l(2, "b"), l(3, "c")];
    let u = state_repo::union_lines(&a, &b);
    assert_eq!(u, vec![l(1, "a"), l(2, "b"), l(3, "c")]);
    assert_eq!(state_repo::union_lines(&a, &a), a);
}
