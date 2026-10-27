use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::receipts::Receipt;

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
    if mode() == "working":
        sys.stdout.write("* Pondering (3s . esc to interrupt)\r\n")
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
        if c == "\x1b" and not paste:
            with open("escaped.txt", "a") as f:
                f.write("esc\n")
            open("mode", "w").write("ready")
            if os.path.exists("restore.txt"):
                buf = open("restore.txt").read()
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

fn rig(tag: &str, mode: &str) -> Option<Rig> {
    use crate::creds::CredSource;
    if !have_python() {
        eprintln!("skipping: python3 missing");
        return None;
    }
    let home = std::env::temp_dir().join(format!("godterm-receipts-{tag}-{}", std::process::id()));
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
    let cfg = crate::config::Config {
        claude_bin: Some(sh.to_string_lossy().into_owned()),
        auto_trust: false,
        ..Default::default()
    };
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
    let transcript = dir.join("sess-r.jsonl");
    std::fs::write(&transcript, "").unwrap();
    {
        let tab = &mut app.panes[0].tabs[0];
        tab.cwd = work.clone();
        tab.session_id = Some("sess-r".into());
        tab.custom_name = Some("claudego".into());
    }
    app.launch_tab(0, 0, crate::pane::LaunchKind::Normal);
    let uid = app.panes[0].tabs[0].uid;
    let mut r = Rig {
        app,
        home,
        work,
        uid,
        transcript,
    };
    let want = if mode == "working" {
        crate::pane::Activity::Working
    } else {
        crate::pane::Activity::Ready
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
        self.app.quiet_tick();
        std::thread::sleep(Duration::from_millis(20));
    }

    fn until(&mut self, secs: u64, what: &str, f: impl Fn(&Rig) -> bool) {
        let t0 = Instant::now();
        while !f(self) {
            assert!(
                t0.elapsed() < Duration::from_secs(secs),
                "timed out waiting for {what}; deliveries: {:?}\nsaid: {:?}\nqueue: {:?}",
                self.app
                    .deliveries
                    .iter()
                    .map(|d| (d.id, d.state_word(), d.stage.clone()))
                    .collect::<Vec<_>>(),
                self.app.assistant.quiet.said,
                self.app
                    .assistant
                    .quiet
                    .queue
                    .iter()
                    .map(|u| u.text.clone())
                    .collect::<Vec<_>>()
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

    fn user(&self, text: &str) {
        self.record(json!({"type": "user", "timestamp": now_ts(), "message": {"role": "user", "content": text}}));
    }

    fn reply_and_end(&self, text: &str) {
        self.record(json!({"type": "assistant", "timestamp": now_ts(), "message": {"id": "m1", "content": [{"type": "text", "text": text}]}}));
        self.record(json!({"type": "system", "subtype": "turn_duration", "timestamp": now_ts()}));
    }

    fn queue_op(&self, op: &str, content: &str, reason: Option<&str>) {
        let mut v = json!({"type": "queue-operation", "operation": op, "timestamp": now_ts(), "content": content});
        if let Some(r) = reason {
            v["reason"] = json!(r);
        }
        self.record(v);
    }

    fn state(&self, id: u64) -> &'static str {
        self.app
            .deliveries
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.state_word())
            .unwrap_or("gone")
    }

    fn submitted(&self) -> String {
        std::fs::read_to_string(self.work.join("submitted.txt")).unwrap_or_default()
    }

    fn said(&self) -> String {
        self.app.assistant.quiet.said.join(" | ")
    }

    fn queued_updates(&self) -> String {
        self.app
            .assistant
            .quiet
            .queue
            .iter()
            .map(|u| u.text.clone())
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

#[test]
fn idle_tab_goes_queued_started_done() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("idle", "working") else {
        return;
    };
    let text = "Alice's request: add a changelog entry";
    let id = r.app.deliver_request(r.uid, text);
    r.step();
    assert_eq!(r.state(id), "queued", "waits while the tab works");
    let st = r.app.control_call("get_state", &json!({}));
    let ds = st["result"]["deliveries"].as_array().unwrap().clone();
    assert_eq!(ds[0]["id"], json!(id));
    assert_eq!(ds[0]["state"], "queued");
    assert!(ds[0]["request"].as_str().unwrap().contains("changelog"));

    r.mode("ready");
    r.until(20, "the paste", |r| r.submitted().contains("changelog"));
    r.mode("working");
    r.user(text);
    r.until(10, "started", |r| r.state(id) == "started");
    r.until(5, "started said", |r| {
        r.said().contains("claudego started on it")
    });
    r.reply_and_end("Added it.");
    r.mode("ready");
    r.until(10, "done", |r| r.state(id) == "done");
    let st = r.app.control_call("get_state", &json!({}));
    assert_eq!(st["result"]["deliveries"][0]["state"], "done");
    assert!(!r.said().contains("dropped"), "{}", r.said());
}

#[test]
fn busy_tab_enqueue_then_absorbed() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("busy", "ready") else {
        return;
    };
    let text = "also check the windows build";
    let id = r.app.deliver_request(r.uid, text);
    r.until(20, "the paste", |r| r.submitted().contains("windows build"));
    r.mode("working");

    r.app.assistant.quiet.spoke_at = Some(Instant::now());
    r.queue_op("enqueue", text, None);
    r.until(10, "received", |r| r.state(id) == "received");
    r.step();
    assert!(
        r.queued_updates()
            .contains("got your request; it's queued behind its current work"),
        "{}",
        r.queued_updates()
    );
    assert!(
        !r.said().contains("got your request"),
        "held while the user talks"
    );
    r.queue_op("remove", text, Some("absorbed_mid_turn"));
    r.record(json!({"type": "attachment", "timestamp": now_ts(), "attachment": {"type": "queued_command", "prompt": text}}));
    r.until(10, "started", |r| r.state(id) == "started");
    r.step();

    assert!(r.queued_updates().contains("claudego started on it"));
    assert!(!r.queued_updates().contains("got your request"));

    r.app.assistant.quiet.spoke_at = None;
    r.until(5, "said", |r| r.said().contains("claudego started on it"));
    r.reply_and_end("Windows is green too.");
    r.mode("ready");
    r.until(10, "done", |r| r.state(id) == "done");
}

#[test]
fn queue_drop_is_said_and_resent() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("drop", "ready") else {
        return;
    };
    let text = "rename the release notes file";
    let id = r.app.deliver_request(r.uid, text);
    r.until(20, "the paste", |r| r.submitted().contains("release notes"));
    r.mode("working");
    r.queue_op("enqueue", text, None);
    r.until(10, "received", |r| r.state(id) == "received");
    r.app.assistant.quiet.spoke_at = Some(Instant::now());
    r.queue_op("popAll", text, None);
    r.until(15, "dropped", |r| r.state(id) == "dropped");
    r.until(5, "the urgent notice", |r| {
        r.said().contains("Your request to claudego was dropped: 'rename the release notes file'. Say resend to send it again.")
    });
    let st = r.app.control_call("get_state", &json!({}));
    let d = &st["result"]["deliveries"][0];
    assert_eq!(d["state"], "dropped");
    assert_eq!(d["resend"], json!(true));
    assert!(d["reason"].as_str().unwrap().contains("input box"));

    r.mode("ready");
    let v = r.app.control_call("resend", &json!({}));
    assert_eq!(v["resent"]["from"], json!(id), "{v}");
    let new = v["resent"]["id"].as_u64().unwrap();
    r.until(20, "the resend", |r| {
        r.submitted().matches(text).count() == 2
    });
    let old = r.app.deliveries.iter().find(|d| d.id == id).unwrap();
    assert_eq!(old.rc.resent_as, Some(new));

    assert!(!r.app.resend_said("resend"));

    {
        let d = r.app.deliveries.iter_mut().find(|d| d.id == new).unwrap();
        d.rc.sent_at = Some(Instant::now() - Duration::from_secs(60));
    }
    r.until(10, "the resend dropped", |r| r.state(new) == "dropped");
    assert!(r.app.resend_said("send it again"));
    r.until(20, "the voice resend", |r| {
        r.submitted().matches(text).count() == 3
    });
}

#[test]
fn interrupted_turn() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("int", "ready") else {
        return;
    };
    let text = "run the full test suite";
    let id = r.app.deliver_request(r.uid, text);
    r.until(20, "the paste", |r| {
        r.submitted().contains("full test suite")
    });
    r.mode("working");
    r.user(text);
    r.until(10, "started", |r| r.state(id) == "started");
    r.record(json!({"type": "assistant", "timestamp": now_ts(), "message": {"content": [{"type": "tool_use", "name": "Bash"}]}}));
    r.record(json!({"type": "user", "timestamp": now_ts(), "message": {"content": [{"type": "text", "text": "[Request interrupted by user for tool use]"}]}}));
    r.mode("ready");
    r.until(10, "interrupted", |r| r.state(id) == "interrupted");
    r.until(5, "the notice", |r| {
        r.said()
            .contains("Your request to claudego was interrupted before it finished: 'run the full test suite'. Say resend")
    });
}

#[test]
fn pasted_but_never_seen_is_dropped() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("unseen", "ready") else {
        return;
    };
    let text = "bump the version number";
    let id = r.app.deliver_request(r.uid, text);
    r.until(20, "the paste", |r| {
        r.submitted().contains("version number")
    });
    r.until(10, "taken", |r| {
        r.app
            .deliveries
            .iter()
            .any(|d| d.id == id && d.stage == crate::control::Stage::Delivered)
    });
    r.step();
    assert_eq!(r.state(id), "sent");

    {
        let d = r.app.deliveries.iter_mut().find(|d| d.id == id).unwrap();
        d.rc.sent_at = Some(Instant::now() - crate::app_receipts::NOT_SEEN);
    }
    r.until(10, "dropped", |r| r.state(id) == "dropped");
    let d = r.app.deliveries.iter().find(|d| d.id == id).unwrap();
    assert!(d.rc.receipt.reason().unwrap().contains("never showed up"));
    r.until(5, "the notice", |r| {
        r.said().contains("was dropped: 'bump the version number'")
    });
}

#[test]
fn escape_warns_and_reports_the_drop() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("esc", "ready") else {
        return;
    };
    let text = "update the readme screenshots";
    let id = r.app.deliver_request(r.uid, text);
    r.until(20, "the paste", |r| r.submitted().contains("readme"));
    r.mode("working");
    r.queue_op("enqueue", text, None);
    r.until(10, "received", |r| r.state(id) == "received");

    std::fs::write(r.work.join("restore.txt"), text).unwrap();

    r.app.assistant.last_user = "interrupt it".into();
    let v = r.app.control_call(
        "press_key",
        &json!({"tab": crate::control::tab_id(r.uid), "key": "escape"}),
    );
    let warn = v["result"]["warning"].as_str().unwrap_or("").to_string();
    assert!(
        warn.contains(&format!("#{id}")) && warn.contains("Escape can drop it"),
        "{v}"
    );
    r.until(10, "the escape", |r| r.work.join("escaped.txt").exists());
    r.until(15, "dropped", |r| r.state(id) == "dropped");
    let d = r.app.deliveries.iter().find(|d| d.id == id).unwrap();
    assert!(
        d.rc.receipt
            .reason()
            .unwrap()
            .contains("Escape put it back"),
        "{:?}",
        d.rc.receipt
    );
    r.until(5, "the notice", |r| {
        r.said()
            .contains("was dropped: 'update the readme screenshots'")
    });
}

#[test]
fn restart_requeues_or_reports() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("restart", "working") else {
        return;
    };
    let queued = r.app.deliver_request(r.uid, "write the migration guide");
    let ran = r.app.deliver_request(r.uid, "tag the release candidate");
    let lost = r.app.deliver_request(r.uid, "ping the reviewers");
    r.step();
    let sent = chrono::Utc::now().timestamp_millis();
    for d in &mut r.app.deliveries {
        if d.id == ran {
            d.rc.receipt = Receipt::Received;
            d.rc.sent_ms = Some(sent);
            d.stage = crate::control::Stage::Delivered;
        }
    }
    r.queue_op("enqueue", "tag the release candidate", None);
    r.user("tag the release candidate");
    r.app.save_state();
    let mut st = crate::state::AppState::load().unwrap();
    assert_eq!(st.deliveries.len(), 3, "{:?}", st.deliveries);

    for sd in &mut st.deliveries {
        if sd.id == lost {
            sd.tab = 99_999;
        }
    }

    for t in &mut r.app.panes[0].tabs {
        t.kill();
    }
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut next = App::new(r.app.cfg.clone(), tx);
    next.accounts[0].login.source = Some(crate::creds::CredSource::Keychain);
    next.restore(st);
    let get = |a: &App, id: u64| a.deliveries.iter().find(|d| d.id == id).cloned().unwrap();
    let q = get(&next, queued);
    assert!(q.rc.restored && q.stage == crate::control::Stage::Queued);
    assert_eq!(
        get(&next, ran).rc.receipt,
        Receipt::Started,
        "not sent twice"
    );
    assert!(matches!(get(&next, lost).rc.receipt, Receipt::Dropped(_)));
    let notices = next
        .assistant
        .quiet
        .queue
        .iter()
        .map(|u| u.text.clone())
        .chain(next.assistant.quiet.said.iter().cloned())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        notices.contains("Your request to t99999 was dropped: 'ping the reviewers'"),
        "{notices}"
    );
    assert!(notices.contains("queued again"), "{notices}");
    assert!(next.delivery_seq >= lost, "ids are not handed out twice");

    r.app = next;
    r.mode("ready");
    let (s, t) = r.app.find_tab(r.uid).unwrap();
    r.app.launch_tab(s, t, crate::pane::LaunchKind::Normal);
    r.until(30, "the re-queued paste", |r| {
        r.submitted().contains("write the migration guide")
    });
    assert!(!r.submitted().contains("tag the release"));
}

#[test]
fn request_with_image_keeps_receipts() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("image", "ready") else {
        return;
    };
    let (p, w, h) =
        crate::assistant_images::save_png(&crate::assistant_images::testing::png(8, 8)).unwrap();
    r.app
        .assistant
        .images
        .push(crate::assistant_images::Attachment {
            id: 1,
            path: p.clone(),
            w,
            h,
            at: Instant::now(),
        });
    let tid = format!("t{}", r.uid);
    let v = r.app.control_call(
        "send_prompt",
        &json!({"_client": "brain", "tab": tid, "text": "fix the header", "expect_reply": false, "images": [1]}),
    );
    assert!(v["error"].is_null(), "{v}");
    let id = r.app.deliveries.last().unwrap().id;
    let path = p.display().to_string();
    r.until(20, "the paste", |r| r.submitted().contains(&path));
    assert!(r.submitted().contains("fix the header"));
    r.mode("working");
    r.record(json!({"type": "user", "timestamp": now_ts(), "message": {"role": "user", "content": [
        {"type": "text", "text": "[Image #1] fix the header"},
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}
    ]}}));
    r.until(10, "started", |r| r.state(id) == "started");
    r.reply_and_end("Fixed.");
    r.mode("ready");
    r.until(10, "done", |r| r.state(id) == "done");
}

#[test]
fn slash_commands_are_told_from_text_and_screen() {
    use crate::app_receipts::{is_slash, slash_echoed};
    for t in [
        "/effort max",
        "/model opus",
        "  /compact",
        "/mcp__godterm:list",
    ] {
        assert!(is_slash(t), "{t}");
    }
    for t in [
        "/Users/alice/notes.md is wrong",
        "fix /etc/hosts",
        "",
        "/",
        "/ effort",
        "/Effort max",
    ] {
        assert!(!is_slash(t), "{t}");
    }
    let run = "● Done.\n\n> /effort max\n  ⎿  Set effort level to max\n\n╭────╮\n│ >  │\n╰────╯\n";
    assert!(slash_echoed(run, "/effort max"));

    let unsent =
        "● Done.\n\n╭──────────────╮\n│ > /effort max │\n╰──────────────╯\n  ? for shortcuts\n";
    assert!(!slash_echoed(unsent, "/effort max"));
}

#[test]
fn slash_command_applied_is_done() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("slash-done", "ready") else {
        return;
    };
    r.app.panes[0].tabs[0].effort = Some("high".into());
    let id = r.app.deliver_request(r.uid, "/effort max");
    r.until(20, "the paste", |r| r.submitted().contains("/effort max"));
    r.until(10, "taken", |r| {
        r.app
            .deliveries
            .iter()
            .any(|d| d.id == id && d.stage == crate::control::Stage::Delivered)
    });

    r.app.panes[0].tabs[0].effort = Some("max".into());
    {
        let d = r.app.deliveries.iter_mut().find(|d| d.id == id).unwrap();
        d.rc.sent_at = Some(Instant::now() - Duration::from_secs(3));
    }
    r.until(10, "done", |r| r.state(id) == "done");
    assert!(!r.said().contains("dropped"), "{}", r.said());
}

#[test]
fn slash_command_without_trace_is_unconfirmed_not_dropped() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut r) = rig("slash-quiet", "ready") else {
        return;
    };
    let id = r.app.deliver_request(r.uid, "/compact");
    r.until(20, "the paste", |r| r.submitted().contains("/compact"));
    r.until(10, "taken", |r| {
        r.app
            .deliveries
            .iter()
            .any(|d| d.id == id && d.stage == crate::control::Stage::Delivered)
    });
    {
        let d = r.app.deliveries.iter_mut().find(|d| d.id == id).unwrap();
        d.rc.sent_at = Some(Instant::now() - crate::app_receipts::NOT_SEEN);
    }
    r.until(10, "unconfirmed", |r| r.state(id) == "unconfirmed");
    assert!(!r.said().contains("was dropped"), "{}", r.said());
}

#[test]
fn slash_commands_are_told_apart_from_paths_and_text() {
    use crate::app_receipts::is_slash;
    for s in [
        "/effort max",
        "/model opus",
        "  /compact",
        "/mcp",
        "/review-pr 12",
        "/agents:list",
    ] {
        assert!(is_slash(s), "{s}");
    }
    for s in [
        "/Users/alice/x",
        "/tmp/a b",
        "hello /effort",
        "/",
        "/ effort",
        "/Effort",
    ] {
        assert!(!is_slash(s), "{s}");
    }
}

#[test]
fn a_slash_command_counts_as_run_when_its_line_has_an_answer_under_it() {
    use crate::app_receipts::slash_echoed;
    let screen = "some output\n\n> /effort  max\n  ⎿  Set effort level to max\n\n> \n";
    assert!(slash_echoed(screen, "/effort max"));

    assert!(!slash_echoed("> /effort max\n", "/effort max"));
    assert!(!slash_echoed("> /effort max\nnext line\n", "/effort max"));
    assert!(!slash_echoed(screen, "/model opus"));
}
