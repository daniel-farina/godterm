use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::Activity;
use std::time::{Duration, Instant};

const STUB: &str = r#"#!/usr/bin/env python3
import os, sys, tty, time
resumed = "--resume" in sys.argv
mode = "idle" if resumed else open("mode").read().strip()
buf = ""
loading_until = time.time() + (2.0 if resumed else 0)
def draw():
    sys.stdout.write("\x1b[2J\x1b[H")
    if time.time() < loading_until:
        sys.stdout.write("Loading conversation\r\n")
    elif mode == "working":
        sys.stdout.write("* Pondering (3s . esc to interrupt)\r\n")
    elif mode == "approval":
        sys.stdout.write("Do you want to proceed?\r\n> 1. Yes\r\n  2. No\r\n")
    else:
        sys.stdout.write("> " + buf + "\r\n? for shortcuts\r\n")
    sys.stdout.flush()
tty.setraw(0)
import select
paste = False
while True:
    draw()
    r, _, _ = select.select([0], [], [], 0.2)
    if not r:
        continue
    data = os.read(0, 65536)
    if not data:
        break
    s = data.decode("utf-8", "ignore")
    i = 0
    while i < len(s):
        if s.startswith("\x1b[200~", i):
            paste = True; i += 6; continue
        if s.startswith("\x1b[201~", i):
            paste = False; i += 6; continue
        c = s[i]; i += 1
        if c == "\r" and not paste:
            if buf.strip():
                with open("submitted.txt", "a") as f:
                    f.write(buf + "\n")
            buf = ""
        else:
            buf += c
"#;

fn have_python() -> bool {
    std::process::Command::new("python3")
        .arg("-c")
        .arg("")
        .status()
        .is_ok_and(|s| s.success())
}

fn screen(app: &crate::app::App, t: usize) -> String {
    app.panes[0].tabs[t]
        .parser
        .lock()
        .unwrap()
        .screen()
        .contents()
}

fn step(app: &mut crate::app::App) {
    for tab in &mut app.panes[0].tabs {
        tab.tick();
        tab.update_activity();
    }
    app.restore_step();
    app.continue_tick();
    app.control_tick();
    std::thread::sleep(Duration::from_millis(20));
}

fn submitted(home: &std::path::Path, t: usize) -> String {
    std::fs::read_to_string(home.join(format!("w{t}/submitted.txt"))).unwrap_or_default()
}

#[test]
fn after_a_crash_only_the_working_tab_continues_once() {
    use crate::creds::CredSource;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !have_python() {
        eprintln!("skipping: python3 missing");
        return;
    }
    let home = std::env::temp_dir().join(format!("godterm-continue-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    crate::config::testing::set_home(&home);
    let stub = home.join("stub.py");
    std::fs::write(&stub, STUB).unwrap();
    let sh = home.join("stub-claude.sh");
    std::fs::write(
        &sh,
        format!("#!/bin/sh\nexec python3 \"{}\" \"$@\"\n", stub.display()),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let modes = ["working", "idle", "approval"];
    for (i, m) in modes.iter().enumerate() {
        let w = home.join(format!("w{i}"));
        std::fs::create_dir_all(&w).unwrap();
        std::fs::write(w.join("mode"), m).unwrap();
    }

    let cfg = crate::config::Config {
        claude_bin: Some(sh.to_string_lossy().into_owned()),
        restore: "lazy".into(),
        auto_trust: false,
        ..Default::default()
    };
    cfg.save().unwrap();

    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(cfg.clone(), tx);
    app.accounts[0].login.source = Some(CredSource::Keychain);
    app.panes[0].add_tab(home.join("w1"));
    app.panes[0].add_tab(home.join("w2"));
    for t in 0..3 {
        app.panes[0].tabs[t].cwd = home.join(format!("w{t}"));
        app.panes[0].tabs[t].session_id = Some(format!("sess-{t}"));
        app.launch_tab(0, t, crate::pane::LaunchKind::Normal);
    }
    app.panes[0].active = 1;
    let want = [Activity::Working, Activity::Ready, Activity::Permission];
    let t0 = Instant::now();
    while (0..3).any(|t| app.panes[0].tabs[t].activity != want[t]) {
        assert!(t0.elapsed().as_secs() < 30, "stubs never settled");
        for tab in &mut app.panes[0].tabs {
            tab.update_activity();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    app.state_dirty = false;
    app.continue_tick();
    assert!(
        app.state_dirty,
        "a change in who works marks the state for saving"
    );

    app.save_state();
    let st = crate::state::AppState::load().unwrap();
    let marked: Vec<bool> = st.slots[0].tabs.iter().map(|t| t.was_working).collect();
    assert_eq!(marked, vec![true, false, true]);
    for tab in &mut app.panes[0].tabs {
        tab.kill();
    }
    drop(app);
    crate::pane::wait_for_reapers(Duration::from_millis(800));

    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(cfg, tx);
    app.accounts[0].login.source = Some(CredSource::Keychain);
    app.autostart();
    assert_eq!(app.cont.waiting.len(), 2);
    assert!(app
        .cont
        .note_text()
        .is_some_and(|n| n.contains("continued 2 working tabs")));
    let t0 = Instant::now();
    let mut saw_loading = false;
    while !(submitted(&home, 0).contains("GodTerm restarted")
        && submitted(&home, 2).contains("GodTerm restarted"))
    {
        assert!(
            t0.elapsed().as_secs() < 40,
            "never continued; screen:\n{}",
            screen(&app, 0)
        );
        step(&mut app);
        if app.panes[0].tabs[0].is_running() && screen(&app, 0).contains("Loading") {
            saw_loading = true;
            assert!(
                app.deliveries.is_empty(),
                "nothing is typed while it is still loading"
            );
        }
    }
    assert!(saw_loading, "the working tab was started eagerly");

    let t1 = Instant::now();
    while t1.elapsed() < Duration::from_secs(4) {
        step(&mut app);
    }
    assert_eq!(submitted(&home, 0).matches("GodTerm restarted").count(), 1);
    assert_eq!(submitted(&home, 2).matches("GodTerm restarted").count(), 1);
    assert_eq!(submitted(&home, 1), "");
    assert!(app.cont.waiting.is_empty());
    for tab in &mut app.panes[0].tabs {
        tab.kill();
    }
    let _ = std::fs::remove_dir_all(&home);
}
