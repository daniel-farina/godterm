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
    elif m == "steer":
        sys.stdout.write("* Pondering (3s . esc to interrupt)\r\n\r\n> " + buf + "\r\n")
    elif m == "approve":
        sys.stdout.write("Bash command\r\n\r\n   ls -la\r\n   List the files\r\n\r\nDo you want to proceed?\r\n❯ 1. Yes\r\n  2. Yes, and don't ask again for ls commands\r\n  3. No, and tell Claude what to do differently (esc)\r\n")
    elif m == "shell":
        sys.stdout.write("> " + buf + "\r\n? for shortcuts . 1 shell still running\r\n")
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

    cfg.assistant.mode = "off".into();
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
        let w2 = home.join("fotmesh");
        std::fs::create_dir_all(&w2).unwrap();
        std::fs::write(w2.join("mode"), "ready").unwrap();
        let i = app.panes[0].add_tab(w2.clone());
        app.panes[0].tabs[i].custom_name = Some("fotmesh".into());
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
        "working" | "steer" => crate::pane::Activity::Working,
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
        .any(|e| e.text == "you: Please run the unit tests and tell me what fails."));

    assert!(r
        .app
        .deliveries
        .iter()
        .any(|d| d.uid == r.uid && d.text.contains("unit tests")));
}

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

    r.app.toggle_direct();
    assert!(r.app.direct_with(r.uid));
    r.app.toggle_direct();
    assert!(r.app.direct.on.is_none());

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
            .contains("The parser now skips blank lines. That is all. Nothing else changed.")
    });

    assert!(!r.said().contains("table"), "{}", r.said());
    assert!(!r.said().contains("fn skip"), "{}", r.said());
    assert!(!r.said().contains("It showed"), "{}", r.said());
    r.brain_untouched();
}

fn talk(r: &mut Rig, ask: &str, key: &'static str, reply: &str) {
    r.hear(ask);
    r.until(20, "the paste", |r| r.submitted().contains(key));
    r.mode("working");
    r.until(10, "working", |r| {
        r.app.panes[0].tabs[0].activity == crate::pane::Activity::Working
    });
    r.reply(reply);
    r.mode("ready");
    let n = r.app.direct.said.len();
    r.until(10, "the reply said", |r| r.app.direct.said.len() > n);
}

fn panel(r: &Rig) -> Vec<String> {
    r.app.assistant.log.iter().map(|e| e.text.clone()).collect()
}

fn bookkeeping(r: &Rig) -> String {
    let q = &r.app.assistant.quiet;
    let mut all: Vec<String> = r.app.direct.said.clone();
    all.extend(q.said.iter().cloned());
    all.extend(q.queue.iter().map(|u| u.text.clone()));
    all.join(" | ").to_lowercase()
}

#[test]
fn a_long_reply_is_said_in_its_own_words_without_the_assistant() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("verbatim", "ready", false) else {
        return;
    };
    r.app.cfg.assistant.mode = "on".into();
    assert_eq!(r.app.cfg.voice.direct_reply, "auto");
    r.enter();
    let said_before = r.app.direct.said.len();
    talk(
        &mut r,
        "How did the migration go?",
        "migration",
        "\u{2705} The **migration** went fine. It moved 12 tables. The old columns are gone. The index was rebuilt, see [the log](https://example.com/log). Nothing was lost. I also ran the checks twice.\n```sql\nselect 1;\n```\nOptions:\n1. Deploy it now\n2. Wait for the review\n\nWhich?",
    );
    let said = r.app.direct.said[said_before..].join(" | ");
    assert_eq!(
        said,
        "The migration went fine. It moved 12 tables. The old columns are gone. The index was rebuilt, see the log. Nothing was lost. I also ran the checks twice. Options. One: Deploy it now. Two: Wait for the review. Which?"
    );
    assert!(!r.said().contains("says"), "{}", r.said());
    assert!(!r.app.assistant.busy, "no assistant turn");
    assert!(r.app.assistant.brain.is_none(), "the assistant never ran");
    assert!(r.app.assistant.last_turn.is_none(), "no turn was started");
    assert!(
        !panel(&r).iter().any(|t| t.contains("summing it up")),
        "{:?}",
        panel(&r)
    );
    assert!(!r.app.assistant.log.iter().any(|e| matches!(
        e.who,
        crate::app_assistant::Who::User | crate::app_assistant::Who::Reply
    )));
}

#[test]
fn the_panel_shows_each_side_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("panel", "ready", false) else {
        return;
    };
    r.app.cfg.assistant.mode = "on".into();
    r.enter();
    talk(
        &mut r,
        "Is the build green?",
        "build",
        "Yes, the build is green. All 40 checks passed.",
    );
    for _ in 0..40 {
        r.step();
    }
    let lines = panel(&r);
    let has = |what: &str| lines.iter().filter(|t| t.contains(what)).count();
    assert_eq!(has("Is the build green?"), 1, "{lines:?}");
    assert_eq!(has("the build is green"), 1, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|t| t == "claudego: Yes, the build is green. All 40 checks passed."),
        "{lines:?}"
    );
    assert!(lines.iter().any(|t| t == "you: Is the build green?"));
    assert!(
        !lines.iter().any(|t| t.starts_with("Prompt to ")),
        "no sent line: {lines:?}"
    );
    assert_eq!(
        r.app
            .direct
            .said
            .iter()
            .filter(|s| s.contains("build is green"))
            .count(),
        1
    );
    let b = bookkeeping(&r);
    for word in [
        "sent",
        "queued",
        "delivered",
        "got your request",
        "started on it",
    ] {
        assert!(!b.contains(word), "{word}: {b}");
    }
}

#[test]
fn a_queued_message_is_only_shown_quietly() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("quietqueue", "working", false) else {
        return;
    };
    r.enter();
    let n = r.app.direct.said.len();
    r.hear("Mm, I agree with that plan.");
    assert_eq!(r.app.direct.said.len(), n, "{}", r.said());
    let lines = panel(&r);
    assert!(
        lines
            .iter()
            .any(|t| t == "you: Mm, I agree with that plan. (queued: claudego is busy)"),
        "{lines:?}"
    );
    let e = r
        .app
        .assistant
        .log
        .iter()
        .find(|e| e.text.contains("I agree with that plan"))
        .unwrap();
    assert!(e.who == crate::app_assistant::Who::Preamble);
    r.mode("ready");
    r.until(20, "the paste", |r| r.submitted().contains("I agree"));
    for _ in 0..60 {
        r.step();
    }
    let extra = r.app.direct.said[n..]
        .iter()
        .filter(|t| !t.ends_with(" finished."))
        .count();
    assert_eq!(extra, 0, "{}", r.said());
    let b = bookkeeping(&r);
    for word in [
        "sent",
        "queued",
        "delivered",
        "got your request",
        "started on it",
    ] {
        assert!(!b.contains(word), "{word}: {b}");
    }
    assert!(!panel(&r).iter().any(|t| t.starts_with("Prompt to ")));
}

#[test]
fn the_brainstorm_hint_stays_away() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("nohint", "ready", false) else {
        return;
    };
    assert!(r.app.cfg.voice.brainstorm_suggest);
    r.enter();
    for t in [
        "So first and",
        "then also the",
        "and maybe um",
        "I think that",
        "not done",
    ] {
        r.hear(t);
    }
    assert!(r.app.brainstorm.suggested.is_none());
    assert!(
        r.app.brainstorm.said.is_empty(),
        "{:?}",
        r.app.brainstorm.said
    );
    assert!(
        !panel(&r).iter().any(|t| t.contains("more to say")),
        "{:?}",
        panel(&r)
    );
    assert!(!r.said().contains("brainstorm"), "{}", r.said());
    r.hear("Back to the assistant.");
    assert!(r.app.direct.on.is_none());
    assert!(r.app.brainstorm_suggest_check("So first and"));
}

#[test]
fn a_muted_speaker_says_nothing_of_the_reply() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("mutedreply", "ready", false) else {
        return;
    };
    r.enter();
    r.app.told.borrow_mut().fake_speaker = true;
    let spoken =
        |r: &Rig, what: &str| {
            r.app.told.borrow().ring.iter().any(|e| {
                e.text.contains(what) && e.channels.contains(&crate::told::Channel::Spoken)
            })
        };
    talk(&mut r, "Is it done?", "done", "Yes, it is done.");
    assert!(spoken(&r, "Yes, it is done."));
    r.app.cfg.voice.speaker_muted = true;
    assert!(!r.app.will_speak_assistant());
    talk(&mut r, "And the docs?", "docs", "The docs are updated too.");
    assert!(!spoken(&r, "The docs are updated too."));
    assert!(panel(&r)
        .iter()
        .any(|t| t == "claudego: The docs are updated too."));
}

#[test]
fn the_reply_is_said_while_a_background_shell_runs() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("shellreply", "ready", false) else {
        return;
    };
    r.enter();
    r.hear("Do you see the dashboard?");
    r.until(20, "the paste", |r| r.submitted().contains("dashboard"));
    r.mode("working");
    r.until(10, "working", |r| {
        r.app.panes[0].tabs[0].activity == crate::pane::Activity::Working
    });
    r.reply("Yes, I see the dashboard.");
    r.mode("shell");
    r.until(10, "the reply said", |r| {
        r.said().contains("Yes, I see the dashboard.")
    });
    r.brain_untouched();
}

#[test]
fn a_busy_tab_queues_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("busy", "working", false) else {
        return;
    };
    r.enter();
    r.hear("Also add a changelog entry.");
    assert!(!r.said().contains("Queued"), "{}", r.said());
    for _ in 0..20 {
        r.step();
    }
    assert!(!r.submitted().contains("changelog"));
    r.mode("ready");
    r.until(20, "the paste", |r| r.submitted().contains("changelog"));
}

#[test]
fn muting_drops_what_was_still_queued() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("mutedrop", "working", false) else {
        return;
    };
    r.enter();
    r.hear("Also add a changelog entry.");
    r.hear("And bump the version.");
    assert!(!r.said().contains("Queued"), "{}", r.said());
    for _ in 0..10 {
        r.step();
    }
    assert_eq!(r.app.drop_spoken_queue("test"), 2);
    r.mode("ready");
    for _ in 0..60 {
        r.step();
    }
    assert!(!r.submitted().contains("changelog"), "{}", r.submitted());
    assert!(!r.submitted().contains("version"), "{}", r.submitted());
}

fn lines_sent(r: &Rig) -> Vec<String> {
    r.submitted()
        .lines()
        .map(|l| l.trim().to_string())
        .collect()
}

fn spoken_left(r: &Rig) -> usize {
    r.app.deliveries.iter().filter(|d| d.rc.spoken).count()
}

fn pasted_anything() -> bool {
    crate::pane::SENT.with(|s| s.borrow().windows(6).any(|w| w == b"\x1b[200~"))
}

#[test]
fn three_utterances_while_busy_arrive_as_one_paste() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("mergebusy", "working", false) else {
        return;
    };
    r.enter();
    r.hear("First rename the config file");
    r.hear("Then update the docs.");
    r.hear("Also bump the version");
    assert_eq!(spoken_left(&r), 3);
    steps_for(&mut r, 1600);
    assert!(r.submitted().is_empty(), "{}", r.submitted());
    assert_eq!(spoken_left(&r), 3, "all three still wait");
    r.mode("ready");
    r.until(20, "the paste", |r| {
        r.submitted().contains("bump the version")
    });
    steps_for(&mut r, 600);
    assert_eq!(
        lines_sent(&r),
        vec!["First rename the config file. Then update the docs. Also bump the version."]
    );
    assert_eq!(spoken_left(&r), 1, "one delivery carries all three");
    assert!(!r.app.deliveries.iter().any(|d| d.rc.mid_turn));
    assert!(!r.said().to_lowercase().contains("queued"), "{}", r.said());
}

#[test]
fn a_burst_is_typed_in_mid_turn_as_one_message() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("steer", "steer", false) else {
        return;
    };
    r.enter();
    steps_for(&mut r, 500);
    let n = r.app.direct.said.len();
    let t0 = Instant::now();
    r.hear("Use the staging database instead.");
    r.hear("And skip the slow tests");
    assert_eq!(spoken_left(&r), 2);
    while t0.elapsed() < Duration::from_millis(700) {
        r.step();
        if t0.elapsed() < Duration::from_millis(700) {
            assert!(
                r.submitted().is_empty(),
                "inside the window: {}",
                r.submitted()
            );
        }
    }
    r.until(10, "the mid-turn paste", |r| {
        r.submitted().contains("slow tests")
    });
    assert!(
        t0.elapsed() < Duration::from_secs(8),
        "it went in right away, not after the turn"
    );
    assert_eq!(
        r.app.panes[0].tabs[0].activity,
        crate::pane::Activity::Working,
        "the tab was still working"
    );
    assert_eq!(
        lines_sent(&r),
        vec!["Use the staging database instead. And skip the slow tests."]
    );
    assert_eq!(spoken_left(&r), 1);
    assert!(r.app.deliveries.iter().all(|d| d.rc.mid_turn));
    let lines = panel(&r);
    assert!(
        lines.iter().any(|t| t == "you: And skip the slow tests"),
        "{lines:?}"
    );
    assert!(!lines.iter().any(|t| t.contains("(queued")), "{lines:?}");
    assert_eq!(r.app.direct.said.len(), n, "{}", r.said());
    r.brain_untouched();
}

#[test]
fn a_cli_that_takes_no_input_mid_turn_waits_for_the_turn_to_end() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("steergrok", "steer", false) else {
        return;
    };
    r.app.panes[0].tabs[0].harness = crate::harness::Harness::Grok;
    r.enter();
    crate::pane::SENT.with(|s| s.borrow_mut().clear());
    r.hear("Use the staging database instead.");
    steps_for(&mut r, 2500);
    assert!(!pasted_anything());
    assert!(r.submitted().is_empty(), "{}", r.submitted());
    assert!(panel(&r)
        .iter()
        .any(|t| t == "you: Use the staging database instead. (queued: claudego is busy)"));
    r.mode("ready");
    r.until(20, "the paste", |r| r.submitted().contains("staging"));
}

#[test]
fn nothing_is_typed_into_an_approval_prompt_and_it_goes_out_merged_after() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("steerapprove", "approve", false) else {
        return;
    };
    r.enter();
    crate::pane::SENT.with(|s| s.borrow_mut().clear());
    r.hear("Use the staging database instead.");
    assert!(
        r.said().contains("It still needs your yes or no first."),
        "{}",
        r.said()
    );
    r.hear("And skip the slow tests.");
    steps_for(&mut r, 2500);
    assert!(!pasted_anything(), "nothing was typed into the prompt");
    assert!(crate::pane::SENT.with(|s| s.borrow().is_empty()));
    assert!(r.answered().is_empty(), "{}", r.answered());
    assert!(r.submitted().is_empty(), "{}", r.submitted());
    assert_eq!(spoken_left(&r), 2);
    assert!(r
        .app
        .deliveries
        .iter()
        .all(|d| d.stage == crate::control::Stage::Queued));
    assert!(panel(&r)
        .iter()
        .any(|t| t == "you: And skip the slow tests. (queued: claudego waits for your yes or no)"));

    r.hear("Yes.");
    r.until(5, "answered", |r| r.answered().contains("'1'"));
    r.until(10, "working on", |r| {
        r.app.panes[0].tabs[0].activity == crate::pane::Activity::Working
    });
    r.mode("steer");
    let t0 = Instant::now();
    r.until(20, "the merged paste", |r| {
        r.submitted().contains("slow tests")
    });
    assert!(
        t0.elapsed() < Duration::from_secs(7),
        "the spoken yes does not count as typing in the tab"
    );
    assert_eq!(
        lines_sent(&r),
        vec!["Use the staging database instead. And skip the slow tests."]
    );
    assert_eq!(r.answered().lines().count(), 1, "{}", r.answered());
    assert_eq!(
        r.app.panes[0].tabs[0].activity,
        crate::pane::Activity::Working
    );
    assert_eq!(spoken_left(&r), 1);
}

#[test]
fn a_prompt_that_comes_up_after_the_paste_gets_no_enter() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("steerrace", "steer", false) else {
        return;
    };
    r.enter();
    steps_for(&mut r, 500);
    r.hear("Use the staging database instead.");
    assert_eq!(spoken_left(&r), 1);
    r.mode("approve");
    r.app.deliveries[0].rc.go = true;
    r.app.deliveries[0].rc.mid_turn = true;
    r.app.deliveries[0].stage = crate::control::Stage::Pasted(Instant::now());
    r.until(10, "the prompt", |r| {
        r.app.panes[0].tabs[0].activity == crate::pane::Activity::Permission
    });
    steps_for(&mut r, 4000);
    assert!(r.answered().is_empty(), "no Enter went into the prompt");
    assert!(r.submitted().is_empty(), "{}", r.submitted());
    assert!(matches!(
        r.app.deliveries[0].stage,
        crate::control::Stage::Failed(_)
    ));
}

#[test]
fn muting_drops_what_waits_in_the_window() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("steermute", "steer", false) else {
        return;
    };
    r.enter();
    steps_for(&mut r, 500);
    r.hear("Use the staging database instead.");
    r.hear("And skip the slow tests.");
    steps_for(&mut r, 200);
    assert_eq!(spoken_left(&r), 2);
    assert_eq!(r.app.drop_spoken_queue("test"), 2);
    assert!(r.app.deliveries.is_empty());
    steps_for(&mut r, 3000);
    assert!(r.submitted().is_empty(), "{}", r.submitted());
    r.mode("ready");
    steps_for(&mut r, 1000);
    assert!(r.submitted().is_empty(), "{}", r.submitted());
}

#[test]
fn a_steered_turn_is_answered_once_and_its_receipts_end() {
    use crate::receipts::Receipt;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("steerreply", "ready", false) else {
        return;
    };
    r.enter();
    let a = "Refactor the parser module please.";
    let b = "Keep the public names as they are.";
    r.hear(a);
    r.until(20, "the paste", |r| r.submitted().contains("Refactor"));
    r.record(json!({"type": "user", "timestamp": now_ts(), "message": {"content": a}}));
    r.mode("steer");
    r.until(10, "working", |r| {
        r.app.panes[0].tabs[0].activity == crate::pane::Activity::Working
    });
    r.hear(b);
    r.until(10, "the mid-turn paste", |r| {
        r.submitted().contains("public names")
    });
    assert_eq!(
        r.app.panes[0].tabs[0].activity,
        crate::pane::Activity::Working
    );
    r.record(json!({"type": "queue-operation", "operation": "enqueue", "timestamp": now_ts(), "content": b}));
    r.record(json!({"type": "attachment", "timestamp": now_ts(), "attachment": {"type": "queued_command", "prompt": b}}));
    steps_for(&mut r, 1000);
    r.reply("The parser is refactored and the public names are kept.");
    r.record(json!({"type": "system", "subtype": "turn_duration", "timestamp": now_ts()}));
    r.mode("ready");
    r.until(10, "the reply said", |r| {
        r.said().contains("the public names are kept")
    });
    r.until(15, "both receipts done", |r| {
        spoken_left(r) == 2
            && r.app
                .deliveries
                .iter()
                .all(|d| d.rc.receipt == Receipt::Done)
    });
    steps_for(&mut r, 1500);
    assert_eq!(
        r.app
            .direct
            .said
            .iter()
            .filter(|s| s.contains("public names are kept"))
            .count(),
        1,
        "{}",
        r.said()
    );
    assert_eq!(lines_sent(&r), vec![a, b]);
    let b = bookkeeping(&r);
    for word in [
        "queued",
        "delivered",
        "got your request",
        "started on it",
        "dropped",
    ] {
        assert!(!b.contains(word), "{word}: {b}");
    }
    assert!(!panel(&r).iter().any(|t| t.starts_with("Prompt to ")));
    r.brain_untouched();
}

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

#[test]
fn talk_to_another_tab_switches() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("switch", "ready", true) else {
        return;
    };
    let w2 = r.home.join("fotmesh");
    r.until(20, "fotmesh ready", |r| {
        r.app.panes[0].tabs[1].activity == crate::pane::Activity::Ready
    });
    r.enter();
    let other = r.app.panes[0].tabs[1].uid;
    r.hear("Talk to fotmesh.");
    assert!(r.app.direct_with(other), "{}", r.said());
    assert!(
        r.said().contains("You're now talking to fotmesh directly."),
        "{}",
        r.said()
    );
    r.hear("Rebuild the index please.");
    r.until(20, "the paste in fotmesh", |_| {
        std::fs::read_to_string(w2.join("submitted.txt"))
            .unwrap_or_default()
            .contains("Rebuild the index")
    });
    assert!(!r.submitted().contains("Rebuild"));
    assert!(
        !r.submitted().contains("fotmesh"),
        "the switch is not typed in"
    );

    assert!(r
        .app
        .assistant
        .direct_note
        .as_deref()
        .is_some_and(|n| n.contains("claudego")));
}

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

fn follow_rig(tag: &str, mode: &str, follow: bool) -> Option<(Rig, u64)> {
    let mut r = rig(tag, mode, true)?;
    r.until(20, "the second tab ready", |r| {
        r.app.panes[0].tabs[1].activity == crate::pane::Activity::Ready
    });
    r.app.cfg.voice.direct_follow = follow;
    r.enter();
    r.step();
    assert_eq!(r.app.panes[0].active, 0);
    let other = r.app.panes[0].tabs[1].uid;
    Some((r, other))
}

fn steps_for(r: &mut Rig, ms: u64) {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(ms) {
        r.step();
    }
}

#[test]
fn follow_is_off_by_default() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert!(!crate::config::Config::default().voice.direct_follow);
    let Some((mut r, _)) = follow_rig("follow-off", "ready", false) else {
        return;
    };
    r.app.jump_to(0, 1);
    steps_for(&mut r, 1200);
    assert!(r.app.direct_with(r.uid), "it stays with its tab");
    assert!(r.app.direct.picked.is_none());
    assert!(!r.said().contains("Now talking"), "{}", r.said());
}

#[test]
fn follow_moves_to_the_tab_the_user_went_to() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some((mut r, other)) = follow_rig("follow-move", "ready", true) else {
        return;
    };
    let t0 = Instant::now();
    r.app.jump_to(0, 1);
    r.until(5, "the move", |r| r.app.direct_with(other));
    assert!(
        t0.elapsed() >= crate::app_direct::FOLLOW_SETTLE,
        "not before the settle time"
    );
    assert!(
        r.said().ends_with("Now talking to fotmesh."),
        "{}",
        r.said()
    );
    assert!(r
        .app
        .assistant
        .log
        .iter()
        .any(|e| e.text == "Voice followed the active tab: now talking to fotmesh"));
    assert!(r.app.modes_line().contains("DIRECT MODE with t"));
    let w2 = r.home.join("fotmesh");
    r.hear("Rebuild the index please.");
    r.until(20, "the paste in the new tab", |_| {
        std::fs::read_to_string(w2.join("submitted.txt"))
            .unwrap_or_default()
            .contains("Rebuild the index")
    });
    assert!(!r.submitted().contains("Rebuild"));

    r.hear("Back to the assistant.");
    assert!(r.app.direct.on.is_none(), "the exit phrase still works");
    r.app.jump_to(0, 0);
    steps_for(&mut r, 1000);
    assert!(
        r.app.direct.on.is_none(),
        "nothing to follow when it is off"
    );
}

#[test]
fn follow_waits_for_the_flipping_to_settle() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some((mut r, other)) = follow_rig("follow-settle", "ready", true) else {
        return;
    };
    r.app.jump_to(0, 1);
    steps_for(&mut r, 200);
    r.app.jump_to(0, 0);
    steps_for(&mut r, 1200);
    assert!(r.app.direct_with(r.uid), "back before it settled");
    assert!(!r.said().contains("Now talking"), "{}", r.said());

    for t in [1, 0, 1, 0, 1] {
        r.app.jump_to(0, t);
        steps_for(&mut r, 150);
        assert!(r.app.direct_with(r.uid), "not while flipping");
    }
    r.until(5, "the move", |r| r.app.direct_with(other));
    steps_for(&mut r, 300);
    assert_eq!(
        r.said().matches("Now talking to").count(),
        1,
        "{}",
        r.said()
    );
}

#[test]
fn follow_waits_while_a_message_is_being_typed_in() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some((mut r, other)) = follow_rig("follow-paste", "working", true) else {
        return;
    };
    r.hear("Also add a changelog entry.");
    assert_eq!(r.app.deliveries.len(), 1);
    r.app.deliveries[0].stage = crate::control::Stage::Pasted(Instant::now());
    r.app.jump_to(0, 1);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(1200) {
        r.app.direct_tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(r.app.direct_with(r.uid), "not in the middle of a paste");
    assert!(r.app.direct.held);
    r.app.deliveries[0].stage = crate::control::Stage::Queued;
    r.app.direct_tick();
    assert!(r.app.direct_with(other), "{}", r.said());

    assert_eq!(r.app.deliveries.len(), 1, "what was queued stays queued");
    assert_eq!(r.app.deliveries[0].uid, r.uid);
    r.mode("ready");
    r.until(20, "the old tab still gets it", |r| {
        r.submitted().contains("changelog")
    });
    let w2 = r.home.join("fotmesh");
    assert!(!std::fs::read_to_string(w2.join("submitted.txt"))
        .unwrap_or_default()
        .contains("changelog"));
}

#[test]
fn follow_waits_while_an_approval_is_being_confirmed() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some((mut r, other)) = follow_rig("follow-confirm", "ready", true) else {
        return;
    };
    r.app.direct.on.as_mut().unwrap().confirm = Some(crate::app_direct::DirectConfirm {
        plan: json!({}),
        choice: "no",
        at: Instant::now(),
    });
    r.app.jump_to(0, 1);
    steps_for(&mut r, 1200);
    assert!(r.app.direct_with(r.uid), "not while a yes is awaited");
    r.app.direct.on.as_mut().unwrap().confirm = None;
    r.until(5, "the move", |r| r.app.direct_with(other));
}

#[test]
fn follow_skips_a_tab_that_is_not_running() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some((mut r, _)) = follow_rig("follow-idle", "ready", true) else {
        return;
    };
    let w3 = r.home.join("idle");
    std::fs::create_dir_all(&w3).unwrap();
    let i = r.app.panes[0].add_tab(w3);
    assert_eq!(r.app.panes[0].active, i);
    steps_for(&mut r, 1200);
    assert!(r.app.direct_with(r.uid), "an idle tab cannot take it");
    assert!(r.app.direct.picked.is_none());
    assert!(!r.said().contains("Now talking"), "{}", r.said());
}

#[test]
fn follow_takes_the_tab_picked_in_a_browser_and_the_web_sets_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some((mut r, other)) = follow_rig("follow-web", "ready", false) else {
        return;
    };
    let v = r.app.web_state();
    assert_eq!(v["voice"]["direct_follow"], json!(false));
    assert_eq!(v["mode_flags"]["direct_tab"], json!(r.uid));
    assert_eq!(v["tabs"][1]["can_direct"], json!(true));

    r.app.web_event(crate::web::ToApp::Viewing {
        client: 7,
        uid: other,
    });
    steps_for(&mut r, 1000);
    assert!(r.app.direct_with(r.uid), "follow is off");

    r.app
        .web_voice_ctl(7, &json!({"direct": r.uid, "follow": true}));
    assert!(r.app.cfg.voice.direct_follow);
    let saved = std::fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(saved.contains("direct_follow = true"), "{saved}");
    assert_eq!(r.app.web_state()["voice"]["direct_follow"], json!(true));

    r.app.web_event(crate::web::ToApp::Viewing {
        client: 7,
        uid: other,
    });
    r.until(5, "the move", |r| r.app.direct_with(other));
    assert_eq!(r.app.panes[0].active, 0, "the app's own view is left alone");
    assert_eq!(r.app.web_state()["mode_flags"]["direct_tab"], json!(other));

    r.app.web_event(crate::web::ToApp::Viewing {
        client: 8,
        uid: r.uid,
    });
    steps_for(&mut r, 100);
    r.app.web_event(crate::web::ToApp::Viewing {
        client: 7,
        uid: other,
    });
    steps_for(&mut r, 1100);
    assert!(r.app.direct_with(other), "the last pick wins");

    r.app.web_voice_ctl(7, &json!({"follow": false}));
    assert!(!r.app.cfg.voice.direct_follow);
    r.app.web_event(crate::web::ToApp::Viewing {
        client: 8,
        uid: r.uid,
    });
    steps_for(&mut r, 1000);
    assert!(r.app.direct_with(other), "sticky again");
    r.app.web_voice_ctl(8, &json!({"direct": r.uid}));
    assert!(r.app.direct_with(r.uid), "Move here still moves it");
}
