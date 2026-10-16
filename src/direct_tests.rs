//! Direct mode end to end: a stub claude in a real PTY (ready, working or
//! asking for approval from a `mode` file in its folder; it records what
//! is submitted and which key answered a prompt) while the test writes the
//! transcript records claude would. Utterances come in as the voice
//! engine's Heard events; nothing opens a mic, no brain runs (the
//! assistant is off), and what would be said is read from direct.said.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::voice::VoiceEvent;

const STUB: &str = r#"#!/usr/bin/env python3
import os, sys, tty, select
buf = ""
def mode():
    try:
        return open("mode").read().strip()
    except Exception:
        return "ready"
def draw():
    sys.stdout.write("\x1b[2J\x1b[H")
    m = mode()
    if m == "working":
        sys.stdout.write("* Pondering (3s . esc to interrupt)\r\n")
    elif m == "approve":
        sys.stdout.write("Bash command\r\n\r\n   ls -la\r\n   List the files\r\n\r\nDo you want to proceed?\r\n❯ 1. Yes\r\n  2. Yes, and don't ask again for ls commands\r\n  3. No, and tell Claude what to do differently (esc)\r\n")
    else:
        sys.stdout.write("> " + buf + "\r\n? for shortcuts\r\n")
    sys.stdout.flush()
tty.setraw(0)
paste = False
while True:
    draw()
    r, _, _ = select.select([0], [], [], 0.1)
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
        if mode() == "approve" and not paste:
            if c in "123\x1b\r":
                with open("answered.txt", "a") as f:
                    f.write(repr(c) + "\n")
                open("mode", "w").write("working")
            continue
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

struct Rig {
    app: App,
    home: PathBuf,
    work: PathBuf,
    uid: u64,
    transcript: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        for p in &mut self.app.panes {
            for t in &mut p.tabs {
                t.kill();
            }
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// An app whose tab "claudego" runs the stub (with a transcript), and
/// with `second`, a tab "botmesh" too.
fn rig(tag: &str, mode: &str, second: bool) -> Option<Rig> {
    use crate::creds::CredSource;
    if !have_python() {
        eprintln!("skipping: python3 missing");
        return None;
    }
    let home = std::env::temp_dir().join(format!("godterm-direct-{tag}-{}", std::process::id()));
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
    let work = home.join("claudego");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("mode"), mode).unwrap();
    let mut cfg = crate::config::Config {
        claude_bin: Some(sh.to_string_lossy().into_owned()),
        auto_trust: false,
        ..Default::default()
    };
    // No brain: nothing may reach one, and none is started.
    cfg.assistant.mode = "off".into();
    cfg.voice.direct_reply = "sentences".into();
    cfg.voice.direct_sentences = 2;
    cfg.save().unwrap();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = App::new(cfg, tx);
    app.accounts[0].login.source = Some(CredSource::Keychain);
    let a = app.panes[0].account.unwrap();
    let dir = app.cfg.accounts[a]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(&work));
    std::fs::create_dir_all(&dir).unwrap();
    let transcript = dir.join("sess-d.jsonl");
    std::fs::write(&transcript, "").unwrap();
    {
        let tab = &mut app.panes[0].tabs[0];
        tab.cwd = work.clone();
        tab.session_id = Some("sess-d".into());
        tab.custom_name = Some("claudego".into());
    }
    app.launch_tab(0, 0, crate::pane::LaunchKind::Normal);
    if second {
        let w2 = home.join("botmesh");
        std::fs::create_dir_all(&w2).unwrap();
        std::fs::write(w2.join("mode"), "ready").unwrap();
        let i = app.panes[0].add_tab(w2.clone());
        app.panes[0].tabs[i].custom_name = Some("botmesh".into());
        app.launch_tab(0, i, crate::pane::LaunchKind::Normal);
    }
    let uid = app.panes[0].tabs[0].uid;
    let mut r = Rig {
        app,
        home,
        work,
        uid,
        transcript,
    };
    let want = match mode {
        "working" => crate::pane::Activity::Working,
        "approve" => crate::pane::Activity::Permission,
        _ => crate::pane::Activity::Ready,
    };
    r.until(20, "the stub to start", |r| {
        r.app.panes[0].tabs[0].activity == want
    });
    Some(r)
}

fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl Rig {
    fn step(&mut self) {
        for tab in &mut self.app.panes[0].tabs {
            tab.tick();
            tab.update_activity();
        }
        self.app.control_tick();
        self.app.direct_tick();
        self.app.quiet_tick();
        std::thread::sleep(Duration::from_millis(20));
    }

    fn until(&mut self, secs: u64, what: &str, f: impl Fn(&Rig) -> bool) {
        let t0 = Instant::now();
        while !f(self) {
            assert!(
                t0.elapsed() < Duration::from_secs(secs),
                "timed out waiting for {what}; said: {:?}; submitted: {:?}; deliveries: {:?}",
                self.app.direct.said,
                self.submitted(),
                self.app
                    .deliveries
                    .iter()
                    .map(|d| (d.id, d.state_word(), d.stage.clone()))
                    .collect::<Vec<_>>(),
            );
            self.step();
        }
    }

    fn mode(&self, m: &str) {
        std::fs::write(self.work.join("mode"), m).unwrap();
    }

    fn record(&self, v: Value) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.transcript)
            .unwrap();
        writeln!(f, "{v}").unwrap();
    }

    fn reply(&self, text: &str) {
        self.record(json!({"type": "assistant", "timestamp": now_ts(), "message": {"id": format!("m{}", now_ts()), "content": [{"type": "text", "text": text}]}}));
    }

    fn submitted(&self) -> String {
        std::fs::read_to_string(self.work.join("submitted.txt")).unwrap_or_default()
    }

    fn answered(&self) -> String {
        std::fs::read_to_string(self.work.join("answered.txt")).unwrap_or_default()
    }

    fn said(&self) -> String {
        self.app.direct.said.join(" | ")
    }

    /// The voice engine heard this (push to talk).
    fn hear(&mut self, text: &str) {
        self.app.on_voice(VoiceEvent::Heard {
            text: text.into(),
            ptt: true,
            stats: Default::default(),
        });
    }

    fn enter(&mut self) {
        let v = self
            .app
            .control_call("talk_to_tab", &json!({"tab": "claudego"}));
        assert_eq!(v["ok"], json!(true), "{v}");
    }

    fn brain_untouched(&self) {
        assert!(!self.app.assistant.busy);
        assert!(
            !self
                .app
                .assistant
                .log
                .iter()
                .any(|e| e.who == crate::app_assistant::Who::User),
            "nothing went to the assistant"
        );
    }
}

/// The assistant's tool starts it, with one line to say; utterances are
/// typed into the tab (paste, then Enter) and never reach the brain.
#[test]
fn the_tool_starts_it_and_speech_goes_to_the_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("enter", "ready", false) else {
        return;
    };
    let v = r
        .app
        .control_call("talk_to_tab", &json!({"tab": "claudego"}));
    assert_eq!(
        v["result"]["say"],
        json!("You're now talking to claudego directly. Say 'back to the assistant' to return.")
    );
    assert!(r.app.direct_with(r.uid));
    assert!(r.app.modes_line().contains("DIRECT MODE with t"));
    r.hear("Please run the unit tests and tell me what fails.");
    r.until(20, "the paste", |r| {
        r.submitted().contains("run the unit tests")
    });
    r.brain_untouched();
    assert!(r
        .app
        .assistant
        .log
        .iter()
        .any(|e| e.text.contains("you → claudego: Please run the unit tests")));
    // A request with a receipt, like any other.
    assert!(r
        .app
        .deliveries
        .iter()
        .any(|d| d.uid == r.uid && d.text.contains("unit tests")));
}

/// "Back to the assistant" is caught here: never typed into the tab.
#[test]
fn an_exit_phrase_is_not_typed_into_the_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("exit", "ready", false) else {
        return;
    };
    r.enter();
    r.hear("Okay, back to the assistant.");
    assert!(r.app.direct.on.is_none());
    assert!(r.said().contains("Back to the assistant."), "{}", r.said());
    for _ in 0..40 {
        r.step();
    }
    assert!(!r.submitted().contains("assistant"), "{}", r.submitted());
    assert!(r.app.deliveries.is_empty());
    assert!(!r.app.modes_line().contains("DIRECT MODE"));
    // Ctrl-a K turns it on for the focused tab and off again.
    r.app.toggle_direct();
    assert!(r.app.direct_with(r.uid));
    r.app.toggle_direct();
    assert!(r.app.direct.on.is_none());
    // So does the tab's menu row.
    use crate::app_tabopts::TabAct;
    let row = r
        .app
        .tab_actions(0, 0)
        .into_iter()
        .find(|x| x.act == TabAct::TalkDirect)
        .unwrap();
    assert_eq!(row.label, "Talk directly by voice");
    r.app.tab_act_run(0, 0, TabAct::TalkDirect);
    assert!(r.app.direct_with(r.uid));
    r.app.tab_act_run(0, 0, TabAct::TalkDirect);
    assert!(r.app.direct.on.is_none());
}

/// When the turn ends its reply is said: the first sentences, code and
/// tables named, not read.
#[test]
fn the_reply_is_said_when_the_turn_ends() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("reply", "ready", false) else {
        return;
    };
    r.enter();
    r.hear("What changed in the parser?");
    r.until(20, "the paste", |r| r.submitted().contains("parser"));
    r.mode("working");
    r.until(10, "working", |r| {
        r.app.panes[0].tabs[0].activity == crate::pane::Activity::Working
    });
    r.reply("The parser now **skips** blank lines:\n```rust\nfn skip() {}\n```\n| a | b |\n|---|---|\nThat is all. Nothing else changed.");
    r.mode("ready");
    r.until(10, "the reply said", |r| {
        r.said()
            .contains("The parser now skips blank lines. It showed some code.")
    });
    // Two sentences (direct_sentences): the table and the rest are not said.
    assert!(!r.said().contains("table"), "{}", r.said());
    assert!(!r.said().contains("fn skip"), "{}", r.said());
    r.brain_untouched();
}

/// A busy tab: the utterance waits in the queue ("Queued." is said) and
/// is typed in once the tab is ready.
#[test]
fn a_busy_tab_queues_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("busy", "working", false) else {
        return;
    };
    r.enter();
    r.hear("Also add a changelog entry.");
    assert!(r.said().contains("Queued."), "{}", r.said());
    for _ in 0..20 {
        r.step();
    }
    assert!(!r.submitted().contains("changelog"));
    r.mode("ready");
    r.until(20, "the paste", |r| r.submitted().contains("changelog"));
}

/// An approval prompt is announced; "yes" answers it (a safe command at
/// once); "no" asks first, as answer_prompt does, then a yes denies it.
#[test]
fn approval_prompts_are_answered_by_voice() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("approve", "ready", false) else {
        return;
    };
    r.enter();
    r.mode("approve");
    r.until(10, "announced", |r| r.said().contains("needs approval"));
    assert!(r.said().contains("Say yes or no."), "{}", r.said());
    r.hear("Yes.");
    r.until(5, "answered", |r| r.answered().contains("'1'"));
    assert!(r.said().contains("Approved."), "{}", r.said());
    assert!(!r.submitted().contains("Yes"), "not typed in");
    // A no asks first.
    r.until(10, "working on", |r| {
        r.app.panes[0].tabs[0].activity == crate::pane::Activity::Working
    });
    r.mode("approve");
    r.until(10, "asked again", |r| {
        r.said().matches("needs approval").count() == 2
    });
    r.hear("No.");
    assert!(r.said().contains("Say yes to go ahead."), "{}", r.said());
    assert!(!r.answered().contains("x1b"));
    r.hear("Yes.");
    r.until(5, "denied", |r| r.answered().lines().count() >= 2);
    assert!(r.said().contains("Denied."), "{}", r.said());
    r.brain_untouched();
}

/// Closing the tab ends direct mode and says so.
#[test]
fn closing_the_tab_ends_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("close", "ready", false) else {
        return;
    };
    r.enter();
    let home = r.home.clone();
    r.app.panes[0].tabs[0].kill();
    r.app.panes[0].close_tab(0, home);
    r.step();
    assert!(r.app.direct.on.is_none());
    assert!(
        r.said()
            .contains("claudego was closed. Back to the assistant."),
        "{}",
        r.said()
    );
    assert!(r
        .app
        .assistant
        .direct_note
        .as_deref()
        .is_some_and(|n| n.contains("claudego")));
}

/// On the way out the assistant gets a short note of what was said, with
/// the next request (and the wake word with a request ends it too).
#[test]
fn the_assistant_gets_a_summary_on_exit() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("summary", "ready", false) else {
        return;
    };
    r.enter();
    r.hear("Is the deploy green?");
    r.until(20, "the paste", |r| r.submitted().contains("deploy"));
    r.mode("working");
    r.until(10, "working", |r| {
        r.app.panes[0].tabs[0].activity == crate::pane::Activity::Working
    });
    r.reply("Yes, the deploy is green. All checks passed.");
    r.mode("ready");
    r.until(10, "the reply said", |r| {
        r.said().contains("deploy is green")
    });
    let ww = r.app.cfg.voice.wake_words[0].clone();
    r.hear(&format!("{ww}, what's my usage?"));
    assert!(r.app.direct.on.is_none(), "the wake word ends it");
    let note = r.app.assistant.direct_note.clone().unwrap_or_default();
    assert!(note.contains("(Direct mode:"), "{note}");
    assert!(note.contains("claudego"), "{note}");
    assert!(note.contains("Is the deploy green?"), "{note}");
    assert!(note.contains("deploy is green"), "{note}");
    assert!(!r.submitted().contains("usage"));
}

/// "talk to botmesh" switches the target; the next utterance goes there.
#[test]
fn talk_to_another_tab_switches() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("switch", "ready", true) else {
        return;
    };
    let w2 = r.home.join("botmesh");
    r.until(20, "botmesh ready", |r| {
        r.app.panes[0].tabs[1].activity == crate::pane::Activity::Ready
    });
    r.enter();
    let other = r.app.panes[0].tabs[1].uid;
    r.hear("Talk to botmesh.");
    assert!(r.app.direct_with(other), "{}", r.said());
    assert!(
        r.said().contains("You're now talking to botmesh directly."),
        "{}",
        r.said()
    );
    r.hear("Rebuild the index please.");
    r.until(20, "the paste in botmesh", |_| {
        std::fs::read_to_string(w2.join("submitted.txt"))
            .unwrap_or_default()
            .contains("Rebuild the index")
    });
    assert!(!r.submitted().contains("Rebuild"));
    assert!(
        !r.submitted().contains("botmesh"),
        "the switch is not typed in"
    );
    // The first tab's part is kept for the assistant.
    assert!(r
        .app
        .assistant
        .direct_note
        .as_deref()
        .is_some_and(|n| n.contains("claudego")));
}

/// Brainstorm mode in direct mode: the draft is for the tab, nothing is
/// typed until "send it", then it goes in as one request.
#[test]
fn brainstorm_drafts_for_the_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("brainstorm", "ready", false) else {
        return;
    };
    r.enter();
    r.hear("Brainstorm mode.");
    assert!(r.app.brainstorm_on());
    assert!(r.app.direct_with(r.uid), "still direct");
    r.hear("First, the parser is slow.");
    r.hear("I'm done with the lexer though.");
    r.hear("Second, the tests are flaky.");
    for _ in 0..30 {
        r.step();
    }
    assert!(r.submitted().is_empty(), "{}", r.submitted());
    assert!(r.app.deliveries.is_empty());
    let chip = r.app.brainstorm_chip().unwrap();
    assert!(chip.contains("to claudego"), "{chip}");
    assert!(r.app.modes_line().contains("brainstorm: on"));
    r.hear("Send it.");
    assert!(!r.app.brainstorm_on());
    r.until(20, "the draft typed in", |r| {
        r.submitted().contains("the tests are flaky")
    });
    let sub = r.submitted();
    assert_eq!(sub.lines().count(), 1, "{sub}");
    assert!(sub.starts_with("First, the parser is slow. I'm done with the lexer though."));
    assert!(!sub.contains("Send it"));
    assert_eq!(r.app.deliveries.len(), 1);
    r.brain_untouched();
    assert!(r.app.direct_with(r.uid));
}
