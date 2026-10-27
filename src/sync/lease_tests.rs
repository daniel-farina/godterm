use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use super::lease::{Op, Outcome, Repo, Status};
use super::takeover::{self, Cmd, Engine, Event};
use super::tests::{config_text, ctx_for, fixture_a, line, root, session_file};
use super::*;

const MIN: i64 = 60_000;

fn link(app_home: &Path, running: &[&str]) -> Link {
    let home = app_home.to_path_buf();
    Link(Arc::new(AppLink {
        running: Mutex::new(running.iter().map(|s| s.to_string()).collect()),
        apply: Box::new(move |a: StoreApply| {
            let s = merge::STORES.iter().find(|s| s.name == a.store).unwrap();
            let p = home.join(s.file);
            let whole: serde_json::Value = std::fs::read(&p)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            write_file(&p, &serde_json::to_vec_pretty(&a.apply_to(&whole)).unwrap()).is_ok()
        }),
    }))
}

struct Two {
    a: Ctx,
    b: Ctx,
    remotes: PathBuf,
    api_a: PathBuf,
    api_b: PathBuf,
}

fn two(name: &str) -> Two {
    let r = root(name);
    let (a, home_a) = fixture_a(&r);
    let remotes = r.join("remotes");
    let rep = restore::init(&a, a.cfg.sync.clone(), &|_| panic!("no question")).unwrap();
    assert!(rep.ok, "{rep:?}");
    let mut a = restore::reload(&a).unwrap();
    let home_b = r.join("b");
    let b = ctx_for(&home_b, &config_text(&remotes, false));
    let o = restore::CloneOpts {
        label: Some("Linux".into()),
        tabs_from: Some("auto".into()),
        ..Default::default()
    };
    let cl = restore::clone(&b, "tester/godterm-state", &o).unwrap();
    assert!(cl.report.ok, "{:?}", cl.report);
    let mut b = restore::reload(&b).unwrap();
    a.machine.label = "MacBook".into();
    a.link = Some(link(&a.app_home, &[]));
    b.link = Some(link(&b.app_home, &[]));
    let now = chrono::Utc::now().timestamp_millis();
    a.now_ms = now;
    b.now_ms = now;
    Two {
        api_a: home_a.join("code/api"),
        api_b: home_b.join("code/api"),
        a,
        b,
        remotes,
    }
}

fn at(c: &Ctx, ms: i64) -> Ctx {
    let mut c = c.clone();
    c.now_ms = ms;
    c
}

fn append(p: &Path, text: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
    std::io::Write::write_all(&mut f, text.as_bytes()).unwrap();
}

fn refs(remotes: &Path) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for owner in std::fs::read_dir(remotes).unwrap().flatten() {
        for repo in std::fs::read_dir(owner.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let out = Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(["for-each-ref", "--format=%(refname) %(objectname)"])
                .output()
                .unwrap();
            for l in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some((r, o)) = l.split_once(' ') {
                    m.insert(format!("{}#{r}", repo.path().display()), o.to_string());
                }
            }
        }
    }
    m
}

fn assert_ff(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) {
    for (k, old) in before {
        let new = after.get(k).unwrap_or_else(|| panic!("{k} disappeared"));
        let (repo, _) = k.split_once('#').unwrap();
        let ok = Command::new("git")
            .args(["-C", repo, "merge-base", "--is-ancestor", old, new])
            .status()
            .unwrap()
            .success();
        assert!(ok, "{k} was rewritten: {old} -> {new}");
    }
}

fn acquire(c: &Ctx, sid: &str, op: Op) -> Outcome {
    let (_, o) = Repo::open(c)
        .unwrap()
        .apply(c, &[(sid.into(), op)], false)
        .unwrap();
    o.into_iter().next().unwrap().1
}

fn engine(c: &Ctx, held: &[&str]) -> Engine {
    let mut e = Engine::default();
    e.online = true;
    for h in held {
        e.held.lock().unwrap().insert(h.to_string(), "work".into());
    }
    let _ = c;
    e
}

#[test]
fn two_clones_race_to_acquire_and_exactly_one_wins() {
    let t = two("lease-race");
    let before = refs(&t.remotes);

    let ra = Repo::open(&t.a).unwrap();
    let rb = Repo::open(&t.b).unwrap();
    let pa = ra
        .prepare(&t.a, &[("S9".into(), Op::Acquire)], false)
        .unwrap();
    let pb = rb
        .prepare(&t.b, &[("S9".into(), Op::Acquire)], false)
        .unwrap();
    assert!(matches!(pa.outcomes[0].1, Outcome::Acquired { .. }));
    assert!(matches!(pb.outcomes[0].1, Outcome::Acquired { .. }));
    assert_eq!(ra.push(&pa).unwrap(), git::Pushed::Done);
    assert_eq!(
        rb.push(&pb).unwrap(),
        git::Pushed::Rejected,
        "B lost the swap"
    );

    match acquire(&t.b, "S9", Op::Acquire) {
        Outcome::Held(Status::Live { owner, label, .. }) => {
            assert_eq!(owner, t.a.machine.id);
            assert_eq!(label, "MacBook");
        }
        o => panic!("{o:?}"),
    }

    for sid in ["R1", "R2", "R3"] {
        let bar = Arc::new(std::sync::Barrier::new(2));
        let hs: Vec<_> = [t.a.clone(), t.b.clone()]
            .into_iter()
            .map(|c| {
                let bar = bar.clone();
                let sid = sid.to_string();
                std::thread::spawn(move || {
                    bar.wait();
                    acquire(&c, &sid, Op::Acquire)
                })
            })
            .collect();
        let outs: Vec<Outcome> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        let won = outs
            .iter()
            .filter(|o| matches!(o, Outcome::Acquired { .. }))
            .count();
        let held = outs
            .iter()
            .filter(|o| matches!(o, Outcome::Held(Status::Live { .. })))
            .count();
        assert_eq!((won, held), (1, 1), "{outs:?}");
    }
    assert_ff(&before, &refs(&t.remotes));
}

#[test]
fn a_lease_expires_after_ten_minutes_and_a_stale_owner_never_blocks() {
    let t = two("lease-expiry");
    let before = refs(&t.remotes);
    let t0 = t.a.now_ms;
    assert!(matches!(
        acquire(&t.a, "S1", Op::Acquire),
        Outcome::Acquired { gen: 1, .. }
    ));

    let b9 = at(&t.b, t0 + 9 * MIN + 59_000);
    assert!(matches!(
        acquire(&b9, "S1", Op::Acquire),
        Outcome::Held(Status::Live { .. })
    ));
    let b10 = at(&t.b, t0 + 10 * MIN);
    match acquire(&b10, "S1", Op::Acquire) {
        Outcome::Acquired {
            gen: 2,
            prior: Status::Stale { owner, .. },
        } => {
            assert_eq!(owner, t.a.machine.id)
        }
        o => panic!("{o:?}"),
    }

    let a11 = at(&t.a, t0 + 11 * MIN);
    assert!(matches!(
        acquire(&a11, "S1", Op::Keep),
        Outcome::Lost(Status::Live { .. })
    ));

    let a12 = at(&t.a, t0 + 12 * MIN);
    assert!(matches!(
        acquire(&a12, "S7", Op::Acquire),
        Outcome::Acquired { .. }
    ));
    let a20 = at(&t.a, t0 + 20 * MIN);
    Repo::open(&a20).unwrap().apply(&a20, &[], true).unwrap();
    let b23 = at(&t.b, t0 + 23 * MIN);
    assert!(matches!(
        acquire(&b23, "S7", Op::Acquire),
        Outcome::Held(Status::Live { .. })
    ));
    assert_ff(&before, &refs(&t.remotes));
}

#[test]
fn take_over_now_on_a_live_lease_old_owner_blocks_and_forks_its_unsent_bytes() {
    let t = two("lease-takeover");
    let before = refs(&t.remotes);
    let mut ea = engine(&t.a, &[]);
    let ev = ea.handle(
        &t.a,
        vec![Cmd::Acquire {
            sid: "S1".into(),
            acct: "work".into(),
            pull: true,
        }],
    );
    assert!(
        matches!(&ev[0], Event::Acquired { offline: false, .. }),
        "{ev:?}"
    );
    assert!(ea.held.lock().unwrap().contains_key("S1"));

    let s1_a = session_file(&t.a, "work", &t.api_a, "S1");
    let before_bytes = std::fs::read(&s1_a).unwrap();
    append(&s1_a, &line("S1", &t.api_a, "unsent on A"));

    let mut eb = engine(&t.b, &[]);
    let ev = eb.handle(
        &t.b,
        vec![Cmd::TakeOverNow {
            sid: "S1".into(),
            acct: "work".into(),
        }],
    );
    assert!(matches!(&ev[0], Event::Acquired { .. }), "{ev:?}");

    let a2 = at(&t.a, t.a.now_ms + 2 * MIN);
    let ev = ea.poll(&a2);
    let lost = ev.iter().find_map(|e| match e {
        Event::Lost {
            sid,
            label,
            conflict,
        } => Some((sid.clone(), label.clone(), *conflict)),
        _ => None,
    });
    assert_eq!(lost, Some(("S1".into(), "Linux".into(), false)), "{ev:?}");

    assert!(!ea.poll(&a2).iter().any(|e| matches!(e, Event::Lost { .. })));

    let mut a_run = a2.clone();
    a_run.link = Some(link(&a2.app_home, &["S1"]));
    let rep = state_repo::sync_now(&a_run).unwrap();
    assert!(rep.ok && rep.held.iter().any(|h| h == "S1"), "{rep:?}");

    let ev = ea.handle(
        &a2,
        vec![Cmd::Fork {
            sid: "S1".into(),
            acct: "work".into(),
            owner: false,
            label: "Linux".into(),
        }],
    );
    let fork = match &ev[0] {
        Event::Forked { fork: Some(f), .. } => f.clone(),
        e => panic!("{e:?}"),
    };
    assert!(!ea.held.lock().unwrap().contains_key("S1"));
    assert_eq!(
        std::fs::read(&s1_a).unwrap(),
        before_bytes,
        "S1 is the synced copy again"
    );
    let fp = session_file(&t.a, "work", &t.api_a, &fork);
    let ft = std::fs::read_to_string(&fp).unwrap();
    assert!(
        ft.contains("unsent on A") && ft.contains("fork from MacBook"),
        "{ft}"
    );

    let rep = state_repo::sync_now(&a2).unwrap();
    assert!(rep.ok, "{rep:?}");
    let m = state_repo::load_manifest(&t.a.state_dir(), "work", &fork).unwrap();
    assert_eq!(m.forked_from.as_deref(), Some("S1"));
    let m1 = state_repo::load_manifest(&t.a.state_dir(), "work", "S1").unwrap();
    assert_eq!(m1.len, before_bytes.len() as u64, "S1 never got A's line");
    assert_ff(&before, &refs(&t.remotes));
}

#[test]
fn a_dead_machine_returning_forks_what_it_never_pushed() {
    let t = two("lease-dead");
    let before = refs(&t.remotes);
    let t0 = t.a.now_ms;
    assert!(matches!(
        acquire(&t.a, "S2", Op::Acquire),
        Outcome::Acquired { .. }
    ));
    let s2_a = session_file(&t.a, "work", &t.api_a, "S2");
    let s2_b = session_file(&t.b, "work", &t.api_b, "S2");

    append(&s2_a, &line("S2", &t.api_a, "A wrote this, then died"));

    let b11 = at(&t.b, t0 + 11 * MIN);
    let mut eb = engine(&b11, &[]);
    let ev = eb.handle(
        &b11,
        vec![Cmd::Acquire {
            sid: "S2".into(),
            acct: "work".into(),
            pull: true,
        }],
    );
    assert!(matches!(&ev[0], Event::Acquired { .. }), "{ev:?}");
    append(&s2_b, &line("S2", &t.api_b, "B carries on"));
    let rb = state_repo::sync_now(&b11).unwrap();
    assert!(rb.ok && rb.sessions_pushed >= 1, "{rb:?}");

    let a30 = at(&t.a, t0 + 30 * MIN);
    let ra = state_repo::sync_now(&a30).unwrap();
    assert!(ra.ok, "{ra:?}");
    assert_eq!(ra.forked.len(), 1, "{ra:?}");
    assert_eq!(std::fs::read(&s2_a).unwrap(), std::fs::read(&s2_b).unwrap());
    let nid = ra.forked[0].split(" -> ").nth(1).unwrap().to_string();
    let f = std::fs::read_to_string(session_file(&t.a, "work", &t.api_a, &nid)).unwrap();
    assert!(f.contains("A wrote this, then died"));

    let ra = state_repo::sync_now(&at(&t.a, t0 + 31 * MIN)).unwrap();
    assert!(ra.ok && ra.forked.is_empty(), "{ra:?}");
    assert_ff(&before, &refs(&t.remotes));
}

#[test]
fn request_then_the_idle_owner_releases_and_the_requester_takes_it() {
    let t = two("lease-request");
    let before = refs(&t.remotes);
    let mut ea = engine(&t.a, &[]);
    ea.handle(
        &t.a,
        vec![Cmd::Acquire {
            sid: "S1".into(),
            acct: "work".into(),
            pull: true,
        }],
    );
    let s1_a = session_file(&t.a, "work", &t.api_a, "S1");
    let s1_b = session_file(&t.b, "work", &t.api_b, "S1");

    append(&s1_a, &line("S1", &t.api_a, "last turn on A"));
    let mut eb = engine(&t.b, &[]);
    let ev = eb.handle(
        &t.b,
        vec![Cmd::Request {
            sid: "S1".into(),
            acct: "work".into(),
        }],
    );
    assert!(
        matches!(
            &ev[0],
            Event::Requested {
                status: Status::Live { .. },
                ..
            }
        ),
        "{ev:?}"
    );

    assert_eq!(eb.interval_s(&t.b), 20);

    let ev = ea.poll(&t.a);
    assert!(
        ev.iter()
            .any(|e| matches!(e, Event::Asked { sid, label } if sid == "S1" && label == "Linux")),
        "{ev:?}"
    );

    let ev = ea.handle(
        &t.a,
        vec![Cmd::Release {
            sid: "S1".into(),
            acct: "work".into(),
        }],
    );
    assert!(matches!(&ev[0], Event::Released { .. }), "{ev:?}");
    let (b, _) = lease::current(&t.a);
    let l = &b.unwrap().leases["S1"];
    let r = l.released.clone().unwrap();
    assert_eq!(r.len, std::fs::metadata(&s1_a).unwrap().len());
    assert_eq!(r.to.as_deref(), Some(t.b.machine.id.as_str()));

    let ev = eb.poll(&t.b);
    assert!(
        ev.iter().any(
            |e| matches!(e, Event::Acquired { sid, pulled: true, forked: None, .. } if sid == "S1")
        ),
        "{ev:?}"
    );
    assert_eq!(std::fs::read(&s1_b).unwrap(), std::fs::read(&s1_a).unwrap());
    assert!(takeover::matches_release(
        &t.b, "work", "S1", r.len, &r.sha256
    ));
    assert!(eb.wanted.is_empty());
    assert_ff(&before, &refs(&t.remotes));
}

#[test]
fn offline_start_resumes_our_lease_with_a_banner_and_forks_on_reconnect() {
    let t = two("lease-offline");
    let before = refs(&t.remotes);

    let mut eb = engine(&t.b, &[]);
    eb.handle(
        &t.b,
        vec![Cmd::Acquire {
            sid: "S1".into(),
            acct: "work".into(),
            pull: true,
        }],
    );
    let mut b_off = t.b.clone();
    b_off.cfg.sync.remote = super::git::file_url(&t.b.home.join("no-network"));
    let _ = std::fs::remove_dir_all(b_off.sync_dir().join("leases.git"));

    let mut eb = engine(&b_off, &[]);
    let ev = eb.handle(
        &b_off,
        vec![Cmd::Acquire {
            sid: "S1".into(),
            acct: "work".into(),
            pull: true,
        }],
    );
    match &ev[0] {
        Event::Acquired {
            offline: true,
            warn: Some(w),
            ..
        } => assert!(w.contains("will fork if someone else took over"), "{w}"),
        e => panic!("{e:?}"),
    }

    acquire(&t.a, "S2", Op::Acquire);
    lease::current(&t.b);
    let ev = eb.handle(
        &b_off,
        vec![Cmd::Acquire {
            sid: "S2".into(),
            acct: "work".into(),
            pull: true,
        }],
    );
    assert!(
        matches!(
            &ev[0],
            Event::Held {
                offline: true,
                status: Status::Live { .. },
                ..
            }
        ),
        "{ev:?}"
    );
    let ev = eb.handle(
        &b_off,
        vec![Cmd::Offline {
            sid: "S2".into(),
            acct: "work".into(),
        }],
    );
    assert!(matches!(&ev[0], Event::Acquired { offline: true, .. }));

    acquire(&t.a, "S1", Op::TakeOverNow);
    let s1_b = session_file(&t.b, "work", &t.api_b, "S1");
    let s2_b = session_file(&t.b, "work", &t.api_b, "S2");
    append(&s1_b, &line("S1", &t.api_b, "written offline on B"));
    append(&s2_b, &line("S2", &t.api_b, "S2 offline on B"));

    let mut b_on = t.b.clone();
    b_on.now_ms += 3 * MIN;
    let ev = eb.poll(&b_on);
    let lost: Vec<&str> = ev
        .iter()
        .filter_map(|e| match e {
            Event::Lost { sid, .. } => Some(sid.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(lost, vec!["S1", "S2"], "{ev:?}");
    for sid in ["S1", "S2"] {
        let ev = eb.handle(
            &b_on,
            vec![Cmd::Fork {
                sid: sid.into(),
                acct: "work".into(),
                owner: false,
                label: "MacBook".into(),
            }],
        );
        assert!(
            matches!(&ev[0], Event::Forked { fork: Some(_), .. }),
            "{ev:?}"
        );
    }
    assert!(!std::fs::read_to_string(&s1_b)
        .unwrap()
        .contains("written offline"));
    assert!(state_repo::sync_now(&b_on).unwrap().ok);
    assert_ff(&before, &refs(&t.remotes));
}

#[test]
fn back_online_with_nobody_else_on_it_keeps_going_without_a_fork() {
    let t = two("lease-offline-quiet");

    let mut b_off = t.b.clone();
    b_off.cfg.sync.remote = super::git::file_url(&t.b.home.join("no-network"));
    let mut eb = engine(&b_off, &[]);
    let ev = eb.handle(
        &b_off,
        vec![Cmd::Acquire {
            sid: "S1".into(),
            acct: "work".into(),
            pull: true,
        }],
    );
    assert!(
        matches!(&ev[0], Event::Acquired { offline: true, .. }),
        "{ev:?}"
    );
    append(
        &session_file(&t.b, "work", &t.api_b, "S1"),
        &line("S1", &t.api_b, "offline"),
    );
    let ev = eb.poll(&t.b);
    assert!(
        !ev.iter().any(|e| matches!(e, Event::Lost { .. })),
        "{ev:?}"
    );
    let (b, _) = lease::current(&t.b);
    assert_eq!(b.unwrap().leases["S1"].owner, t.b.machine.id);

    let r = state_repo::sync_now(&t.b).unwrap();
    assert!(
        r.ok && r.forked.is_empty() && r.sessions_pushed >= 1,
        "{r:?}"
    );
}

#[test]
fn pulled_store_changes_go_through_the_app_never_written_under_it() {
    let t = two("lease-stores");

    let ap = t.b.app_home.join("archive.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&ap).unwrap()).unwrap();
    v["entries"][0]["label"] = json!("docs, renamed on B");
    std::fs::write(&ap, serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(state_repo::sync_now(&t.b).unwrap().ok);
    let seen: Arc<Mutex<Vec<StoreApply>>> = Arc::default();
    let s2 = seen.clone();
    let mut a = t.a.clone();
    a.link = Some(Link(Arc::new(AppLink {
        running: Mutex::default(),
        apply: Box::new(move |x| {
            s2.lock().unwrap().push(x);
            false
        }),
    })));
    let file = a.app_home.join("archive.json");
    let before = std::fs::read(&file).unwrap();
    let r = state_repo::sync_now(&a).unwrap();
    assert_eq!(
        std::fs::read(&file).unwrap(),
        before,
        "never written by the sync"
    );
    assert!(r.errors.iter().any(|e| e.contains("archive")), "{r:?}");
    let got = seen.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].store, "archive");
    assert_eq!(
        got[0].set["a1"].as_ref().unwrap()["label"],
        "docs, renamed on B"
    );

    a.link = Some(link(&a.app_home, &[]));
    let r = state_repo::sync_now(&a).unwrap();
    assert!(r.applied.iter().any(|x| x.starts_with("archive")), "{r:?}");
    let r = state_repo::sync_now(&a).unwrap();
    assert!(!r.applied.iter().any(|x| x.starts_with("archive")), "{r:?}");
    let now: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(now["entries"][0]["label"], "docs, renamed on B");
}

#[test]
fn apply_to_keeps_the_order_and_other_fields() {
    let whole =
        json!({"jobs": [{"id": "a", "n": 1}, {"id": "b", "n": 2}, {"id": "c", "n": 3}], "x": 1});
    let mut set = BTreeMap::new();
    set.insert("b".to_string(), Some(json!({"id": "b", "n": 20})));
    set.insert("a".to_string(), None);
    set.insert("d".to_string(), Some(json!({"id": "d", "n": 4})));
    let out = StoreApply { store: "jobs", set }.apply_to(&whole);
    assert_eq!(
        out,
        json!({"jobs": [{"id": "b", "n": 20}, {"id": "c", "n": 3}, {"id": "d", "n": 4}], "x": 1})
    );
}
