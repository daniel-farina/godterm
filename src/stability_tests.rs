use std::time::Duration;

use crate::config::testing::LOCK as ENV_LOCK;

fn names_on_disk() -> Vec<String> {
    let cfg = crate::config::Config::parse(
        &std::fs::read_to_string(crate::config::Config::path()).unwrap(),
    )
    .unwrap();
    cfg.accounts.into_iter().map(|a| a.name).collect()
}

#[test]
fn removing_an_account_twice_or_after_a_hand_edit_does_not_panic() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = std::env::temp_dir().join(format!("godterm-stab-rm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let mut cfg = crate::config::Config::default();
    cfg.claude_bin = Some(crate::test_stub::true_bin());
    cfg.autostart = false;
    cfg.save().unwrap();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(cfg, tx);

    app.remove_account(3);
    assert_eq!(names_on_disk(), ["account1", "account2", "account3"]);

    app.remove_account(3);
    assert_eq!(names_on_disk(), ["account1", "account2", "account3"]);
    assert!(
        app.flash
            .as_ref()
            .is_some_and(|(m, _)| m.contains("not in config.toml")),
        "{:?}",
        app.flash
    );

    let text = std::fs::read_to_string(crate::config::Config::path()).unwrap();
    let mut c = crate::config::Config::parse(&text).unwrap();
    c.accounts.rotate_left(1);
    c.save().unwrap();
    assert_eq!(names_on_disk(), ["account2", "account3", "account1"]);
    app.remove_account(1);
    assert_eq!(names_on_disk(), ["account3", "account1"]);
    app.remove_account(99);
    assert_eq!(names_on_disk(), ["account3", "account1"]);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_instant_before_the_clock_starts_is_now() {
    let t = crate::clock::ago(Duration::from_secs(5));
    assert!(t.elapsed() >= Duration::from_secs(5) || t.elapsed() < Duration::from_secs(1));

    let t = crate::clock::ago(Duration::MAX);
    assert!(t.elapsed() < Duration::from_secs(1));
}

#[test]
fn ids_and_dates_are_cut_between_chars() {
    let dir = std::env::temp_dir().join(format!("godterm-stab-ids-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    for id in ["abcdefgé.jsonl", "日本語のセッション", "🚀🚀🚀", "a", ""] {
        assert!(!crate::session_ids::is_bg_session(&dir, id), "{id}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn wav(rate: u32) -> Vec<u8> {
    let data: Vec<u8> = (0..64i16).flat_map(|s| (s * 100).to_le_bytes()).collect();
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    b
}

#[test]
fn a_wav_with_a_zero_or_absurd_rate_is_refused_and_resampling_never_divides_by_zero() {
    use crate::voice::engines::wav_samples;
    use crate::voice::player::resample;
    let (s, r) = wav_samples(&wav(24_000)).unwrap();
    assert_eq!((s.len(), r), (64, 24_000));

    assert!(wav_samples(&wav(0)).is_none());
    assert!(wav_samples(&wav(1)).is_none());
    assert!(wav_samples(&wav(u32::MAX / 2)).is_none());
    assert_eq!(resample(&s, 0, 48_000), s);
    assert_eq!(resample(&s, 24_000, 0), s);
    assert_eq!(resample(&s, 24_000, 48_000).len(), 128);

    let w = wav(22_050);
    for cut in 0..w.len() {
        let _ = wav_samples(&w[..cut]);
    }
}

#[test]
fn damaged_voice_files_are_errors_not_panics() {
    use crate::voice::kokoro::{parse_npy, Voices};

    assert!(parse_npy(b"\x93NUMPY\x02\x00\x10\x00").is_err());
    assert!(parse_npy(b"\x93NUMPY\x02\x00\x10\x00\x00").is_err());

    let mut z = vec![0u8; 46];
    z[..4].copy_from_slice(b"PK\x01\x02");
    z[28..30].copy_from_slice(&500u16.to_le_bytes());
    assert!(Voices::parse(&z).is_err());

    let mut x: u64 = 0x1234_5678;
    for n in 0..300 {
        let mut b = Vec::new();
        for _ in 0..(n % 97) * 7 + 46 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            b.push(x as u8);
        }
        for at in [0, b.len() / 2, b.len().saturating_sub(46)] {
            b[at..at + 4].copy_from_slice(b"PK\x01\x02");
        }
        let _ = Voices::parse(&b);
        let _ = parse_npy(&b);
    }
}

#[test]
fn a_vt100_panic_on_a_wide_char_cut_by_a_resize_rebuilds_the_screen() {
    for edit in [&b"\x1b[2;9H\x1b[K"[..], b"\x1b[2;9Hx", b"\x1b[2;9H\x1b[P"] {
        let mut p = vt100::Parser::new(5, 10, 100);
        assert!(crate::pane::process_guarded(
            &mut p,
            "kept line\r\n\x1b[2;9H日".as_bytes(),
            100
        ));
        p.screen_mut().set_size(5, 9);
        let ok = crate::pane::process_guarded(&mut p, edit, 100);
        assert!(!ok, "{edit:?}: the parser panics here");

        let shown = p.screen().contents();
        assert!(shown.contains("kept line"), "{shown:?}");
        assert!(crate::pane::process_guarded(&mut p, b"\x1b[5;1Hafter", 100));
        assert!(p.screen().contents().contains("after"));
        assert_eq!(p.screen().size(), (5, 9));
    }
}
