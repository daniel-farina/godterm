use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serde_json::{json, Value};

use crate::app::{App, AppEvent};
use crate::assistant_images::testing::{png, FakeClipboard};
use crate::assistant_images::{self as img, Attachment, ClipImage};
use crate::config::testing::LOCK as ENV_LOCK;

fn test_app(tag: &str) -> (App, PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-images-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = App::new(crate::config::Config::default(), tx);
    let base = home.join("newtabs");
    std::fs::create_dir_all(&base).unwrap();
    app.cfg.new_tab_base = base.to_string_lossy().into_owned();
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    app.accounts[0].login.source = Some(CredSource::Keychain);
    (app, home)
}

fn fake_clip(app: &mut App, holds: Option<ClipImage>) -> FakeClipboard {
    let c = FakeClipboard::default();
    *c.holds.lock().unwrap() = holds;
    app.assistant.clipboard = Some(Box::new(c.clone()));
    c
}

fn ctrl_v(app: &mut App) {
    app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
}

fn key(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn assert_private(p: &Path, want: u32) {
    use crate::platform::PermissionsExt;
    let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    if cfg!(windows) {
        assert!(mode & 0o200 != 0, "{p:?} is read only");
    } else {
        assert_eq!(mode, want, "{p:?}");
    }
}

#[test]
fn chips_in_the_input() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("chips");
    app.assistant.show = true;
    app.assistant.focused = true;
    let clip = fake_clip(&mut app, Some(ClipImage::Png(png(64, 32))));
    ctrl_v(&mut app);
    assert_eq!(app.assistant.input, "[image 1: 64x32] ");
    let first = app.assistant.pending_images[0].path.clone();
    assert!(first.starts_with(img::dir()) && first.is_file());
    assert_private(&first, 0o600);
    assert_private(&img::dir(), 0o700);
    for c in "fix this".chars() {
        key(&mut app, KeyCode::Char(c));
    }

    let shot = home.join("Screen Shot 1.png");
    std::fs::write(&shot, png(10, 8)).unwrap();
    app.handle(AppEvent::Input(Event::Paste(format!(
        "'{}'",
        shot.display()
    ))));
    assert_eq!(
        app.assistant.input,
        "[image 1: 64x32] fix this [image 2: 10x8] "
    );
    let copy = app.assistant.pending_images[1].path.clone();
    assert!(copy.starts_with(img::dir()), "a private copy: {copy:?}");

    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.assistant.input, "[image 1: 64x32] fix this ");
    assert_eq!(app.assistant.pending_images.len(), 1);
    assert!(!copy.exists() && shot.exists(), "only our copy is removed");

    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.assistant.input, "[image 1: 64x32] fix this");

    let reads = clip.reads.load(std::sync::atomic::Ordering::SeqCst);
    app.handle(AppEvent::Input(Event::Paste(String::new())));
    assert_eq!(
        clip.reads.load(std::sync::atomic::Ordering::SeqCst),
        reads + 1
    );
    assert_eq!(
        app.assistant.input,
        "[image 1: 64x32] fix this [image 3: 64x32] "
    );

    app.handle(AppEvent::Input(Event::Paste("/tmp/notes.txt".into())));
    assert!(app.assistant.input.ends_with("] /tmp/notes.txt"));

    *clip.holds.lock().unwrap() = None;
    let before = app.assistant.input.clone();
    ctrl_v(&mut app);
    assert_eq!(app.assistant.input, before);
    assert!(app
        .flash
        .as_ref()
        .unwrap()
        .0
        .contains("No image on the clipboard"));
    let _ = std::fs::remove_dir_all(&home);
}

fn wait_for(rec: &Path, needle: &str) -> String {
    let t0 = Instant::now();
    loop {
        let s = std::fs::read_to_string(rec).unwrap_or_default();
        if s.contains(needle) || t0.elapsed() > Duration::from_secs(5) {
            return s;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn brain_message_has_the_image_block() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("brain");
    app.cfg.assistant.account = app.cfg.accounts[0].name.clone();
    let rec = home.join("brain-in.txt");
    let stub = crate::test_stub::claude(
        &home.join("brain"),
        &[
            ("stdin_to", rec.display().to_string()),
            ("stdin_append", "1".into()),
        ],
    );
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    app.assistant.show = true;
    app.assistant.focused = true;
    fake_clip(&mut app, Some(ClipImage::Png(png(64, 32))));
    ctrl_v(&mut app);
    for c in "what is this".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    let file = app.assistant.pending_images[0].path.clone();
    app.send_assistant_input();
    let got = wait_for(&rec, "\"image\"");
    let line = got
        .lines()
        .find(|l| l.contains("\"image\""))
        .unwrap_or_else(|| panic!("no image block: {got}"));
    let v: Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["type"], "user");
    let c = v["message"]["content"].as_array().unwrap();
    let text = c[0]["text"].as_str().unwrap();
    assert!(text.contains("[image 1: 64x32] what is this"), "{text}");
    assert!(
        text.contains("image 1 (64x32, shown below)") && text.contains("images [numbers]"),
        "{text}"
    );
    assert_eq!(
        c[1],
        json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": img::base64(&std::fs::read(&file).unwrap())}})
    );
    assert!(app
        .assistant
        .log
        .iter()
        .any(|e| e.text == "\u{1F4CE} image 1 (64x32)"));
    assert!(app.assistant.pending_images.is_empty());
    assert_eq!(app.assistant.images.len(), 1);
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(&home);
}

fn tab(app: &mut App, home: &Path) -> u64 {
    let proj = home.join("newtabs").join("claudego");
    std::fs::create_dir_all(&proj).unwrap();
    let t = app.panes[0].add_tab(proj);
    let tb = &mut app.panes[0].tabs[t];
    tb.custom_name = Some("claudego".into());
    tb.pending = Some(crate::pane::LaunchKind::Normal);
    tb.activity = crate::pane::Activity::Ready;
    tb.uid
}

fn attachment(app: &mut App, id: u32, ago: Duration) -> PathBuf {
    let (p, w, h) = img::save_png(&png(20, 10)).unwrap();
    app.assistant.images.push(Attachment {
        id,
        path: p.clone(),
        w,
        h,
        at: Instant::now().checked_sub(ago).unwrap(),
    });
    p
}

#[test]
fn send_prompt_with_images() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("send");
    let uid = tab(&mut app, &home);
    let tid = format!("t{uid}");
    let p1 = attachment(&mut app, 1, Duration::ZERO);
    let p2 = attachment(&mut app, 2, Duration::from_secs(31 * 60));
    let send = |app: &mut App, images: Value| {
        app.control_call(
            "send_prompt",
            &json!({"_client": "brain", "tab": tid, "text": "Fix the header", "expect_reply": false, "images": images}),
        )
    };
    let v = send(&mut app, json!([1]));
    assert!(v["error"].is_null(), "{v}");
    let last = |app: &App| app.deliveries.last().unwrap().text.clone();
    assert_eq!(last(&app), format!("Fix the header\n\n{}", p1.display()));

    assert_eq!(crate::receipts::key(&last(&app)), "Fix the header");
    let v = send(&mut app, json!(["image 1"]));
    assert!(v["error"].is_null(), "{v}");

    let elsewhere = std::env::temp_dir().join(format!("godterm-images-own-{}", std::process::id()));
    std::fs::create_dir_all(&elsewhere).unwrap();
    let own = elsewhere.join("mock.jpg");
    std::fs::write(&own, [0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
    let v = send(&mut app, json!([1, own.display().to_string()]));
    assert!(v["error"].is_null(), "{v}");
    assert_eq!(
        last(&app),
        format!("Fix the header\n\n{}\n{}", p1.display(), own.display())
    );
    let notes = elsewhere.join("notes.txt");
    std::fs::write(&notes, "not an image").unwrap();
    let private = home.join("x.png");
    std::fs::write(&private, png(2, 2)).unwrap();
    let n = app.deliveries.len();
    for (bad, why) in [
        (json!([9]), "not an attachment"),
        (json!([2]), "over 30 minutes"),
        (json!([notes.display().to_string()]), "not an image file"),
        (json!([private.display().to_string()]), "private"),
    ] {
        let v = send(&mut app, bad);
        assert!(
            v["error"].as_str().unwrap_or("").contains(why),
            "{why}: {v}"
        );
    }
    assert_eq!(app.deliveries.len(), n, "nothing sent for a bad image");
    let _ = p2;

    app.cfg.accounts[0].harness = "grok".into();
    let v = send(&mut app, json!([1]));
    assert!(v["error"].is_null(), "{v}");
    assert_eq!(
        last(&app),
        format!("Fix the header\n\nImage: {}", p1.display())
    );
    assert_eq!(crate::receipts::key(&last(&app)), "Fix the header");

    let a = app.assistant.images[0].clone();
    let (note, blocks) = app.turn_images(&[a]);
    assert!(blocks.is_empty());
    assert!(
        note.contains(&p1.display().to_string()) && note.contains("You cannot see images"),
        "{note}"
    );

    let tools = crate::control::TOOLS;
    for name in ["send_prompt", "open_tab"] {
        let t = tools.iter().find(|t| t.name == name).unwrap();
        assert!((t.schema)()["properties"]["images"].is_object(), "{name}");
    }
    let p: String = crate::sysprompt::SECTIONS
        .iter()
        .map(|x| x.default)
        .collect();
    assert!(p.contains("Never send the user's images to a tab they did not mean"));
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&elsewhere);
}
