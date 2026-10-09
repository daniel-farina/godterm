//! A paste then Enter into a real PTY (a stub claude that, like claude,
//! reads a CR in the same read as a paste as a new line, and is slow to
//! read, as on a loaded machine): voice "send to tab" and the login code
//! paste submit once, never as a stray newline. With the old timed Enter
//! (80 ms after the paste) both came in one read and nothing was sent.

use crate::config::testing::LOCK as ENV_LOCK;

const STUB: &str = r#"#!/usr/bin/env python3
import os, sys, tty, time
buf = ""
def draw():
    sys.stdout.write("\x1b[2J\x1b[Hready> " + buf.replace("\n", " NL ") + "\r\n? for shortcuts\r\n")
    sys.stdout.flush()
tty.setraw(0)
draw()
paste = False
while True:
    # Busy between reads, as claude is redrawing on a loaded machine:
    # whatever arrives meanwhile comes in one read.
    time.sleep(0.4)
    data = os.read(0, 65536)
    if not data:
        break
    s = data.decode("utf-8", "ignore")
    burst = "\x1b[200~" in s
    i = 0
    submit = False
    while i < len(s):
        if s.startswith("\x1b[200~", i):
            paste = True; i += 6; continue
        if s.startswith("\x1b[201~", i):
            paste = False; i += 6; continue
        c = s[i]; i += 1
        if c == "\r" and not paste and not burst:
            submit = True
        elif c in "\r\n":
            buf += "\n"
        else:
            buf += c
    if submit and buf.strip():
        with open("submitted.txt", "a") as f:
            f.write(buf + "\n")
        buf = ""
    draw()
"#;

fn have_python() -> bool {
    std::process::Command::new("python3")
        .arg("-c")
        .arg("")
        .status()
        .is_ok_and(|s| s.success())
}

/// An app whose tab 1 runs the stub, ready for input.
fn started(tag: &str) -> Option<(crate::app::App, std::path::PathBuf)> {
    use crate::creds::CredSource;
    if !have_python() {
        eprintln!("skipping: python3 missing");
        return None;
    }
    let home =
        std::env::temp_dir().join(format!("godterm-pasteenter-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    crate::config::testing::set_home(&home);
    let stub = home.join("stub.py");
    std::fs::write(&stub, STUB).unwrap();
    let sh = home.join("stub-claude.sh");
    std::fs::write(
        &sh,
        format!("#!/bin/sh\nexec python3 \"{}\"\n", stub.display()),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut cfg = crate::config::Config::default();
    cfg.claude_bin = Some(sh.to_string_lossy().into_owned());
    cfg.save().unwrap();
    let mut app = crate::app::App::new(cfg, tx);
    app.accounts[0].login.source = Some(CredSource::Keychain);
    app.panes[0].tabs[0].cwd = work;
    app.launch_tab(0, 0, crate::pane::LaunchKind::Normal);
    let t0 = std::time::Instant::now();
    while !screen(&app).contains("ready>") {
        assert!(t0.elapsed().as_secs() < 60, "the stub never started");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Some((app, home))
}

fn screen(app: &crate::app::App) -> String {
    app.panes[0].tabs[0]
        .parser
        .lock()
        .unwrap()
        .screen()
        .contents()
}

/// Run the app's ticks until the stub recorded `want` (or time is up).
fn submitted(app: &mut crate::app::App, home: &std::path::Path, want: &str) -> String {
    let f = home.join("work/submitted.txt");
    let t0 = std::time::Instant::now();
    while t0.elapsed().as_secs() < 30 {
        app.control_tick();
        let got = std::fs::read_to_string(&f).unwrap_or_default();
        if got.contains(want) {
            return got;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!(
        "never submitted {want:?}; screen:\n{}\nsubmitted: {:?}",
        screen(app),
        std::fs::read_to_string(&f).unwrap_or_default()
    );
}

#[test]
fn a_voice_send_submits_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some((mut app, home)) = started("voice") else {
        return;
    };
    app.send_text(0, 0, "stop the loop\nwhen this turn ends");
    let got = submitted(&mut app, &home, "stop the loop\nwhen this turn ends\n");
    assert_eq!(got.matches("stop the loop").count(), 1, "{got:?}");
    app.panes[0].tabs[0].kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_login_code_paste_submits_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some((mut app, home)) = started("code") else {
        return;
    };
    app.paste_then_enter(0, 0, "AbC123-xyz#state");
    // Enter waits for the echo: never queued twice, gone once pressed.
    assert_eq!(app.pending_enters.len(), 1);
    let got = submitted(&mut app, &home, "AbC123-xyz#state\n");
    assert_eq!(got.matches("AbC123").count(), 1);
    assert!(app.pending_enters.is_empty());
    app.panes[0].tabs[0].kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// No echo at all (a screen that never shows it): Enter anyway, after
/// a while, once.
#[test]
fn no_echo_still_presses_enter() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home =
        std::env::temp_dir().join(format!("godterm-pasteenter-noecho-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    app.paste_then_enter(0, 0, "hidden text");
    app.pending_enters_tick();
    assert_eq!(app.pending_enters.len(), 1, "waits for the echo");
    app.pending_enters[0].at -= std::time::Duration::from_secs(3);
    app.pending_enters_tick();
    assert!(app.pending_enters.is_empty(), "pressed after the wait");
    let _ = std::fs::remove_dir_all(&home);
}
