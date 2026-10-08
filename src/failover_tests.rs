//! Usage aware failover: the notice when an account runs out, a limit
//! message in a tab's output, a move with one yes, auto moving only idle
//! tabs, and moves only between accounts of the same provider.

use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::{Activity, PaneState};

fn usage(five_used: f64, week_used: f64) -> crate::usage::Usage {
    crate::usage::parse_usage(&format!(
        r#"{{"five_hour":{{"utilization":{five_used},"resets_at":"2099-10-09T17:29:00+00:00"}},"seven_day":{{"utilization":{week_used},"resets_at":"2099-10-16T13:00:00+00:00"}}}}"#
    ))
    .unwrap()
}

fn test_app(tag: &str, grok: &[usize]) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-failover-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut cfg = crate::config::Config::default();
    for &g in grok {
        cfg.accounts[g].harness = "grok".into();
    }
    cfg.claude_bin = Some(crate::test_stub::true_bin());
    cfg.save().unwrap();
    let mut app = crate::app::App::new(cfg, tx);
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
        app.accounts[a].usage = Some(usage(10.0, 10.0));
    }
    // Every pane's tab runs (as far as failover can tell).
    for p in &mut app.panes {
        p.tabs[0].state = PaneState::Running;
        p.tabs[0].activity = Activity::Ready;
    }
    (app, home)
}

fn slot_of(app: &crate::app::App, a: usize) -> usize {
    app.panes.iter().position(|p| p.account == Some(a)).unwrap()
}

#[test]
fn a_low_account_offers_a_move_to_the_one_with_most_left() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("low", &[]);
    app.accounts[2].usage = Some(usage(10.0, 100.0));
    app.accounts[3].usage = Some(usage(0.0, 0.0));
    app.failover_scan();
    let uid = app.panes[slot_of(&app, 2)].tabs[0].uid;
    assert_eq!(app.failover.len(), 1, "only the tab on Account 3");
    let o = app.failover[0].clone();
    assert_eq!((o.uid, o.from, o.to), (uid, 2, 3));
    let t = app.failover_text(&o);
    assert!(
        t.starts_with("Account 3 is out for the week (resets ")
            && t.ends_with("Move this session to Account 4 (100% left)?"),
        "{t}"
    );
    // The notice, the assistant's state, nothing moved yet.
    assert!(crate::ui::status_chips(&app)
        .iter()
        .any(|c| c.action == crate::hits::UiAction::FailoverMove && c.text.contains("[C-a F]")));
    assert!(app.failover_state_line().contains(&format!("t{uid}")));
    assert!(app.pending_moves.is_empty() && app.queued_moves.is_empty());
    // Above the threshold again: the offer goes.
    app.accounts[2].usage = Some(usage(10.0, 50.0));
    app.failover_scan();
    assert!(app.failover.is_empty());
    // A threshold of its own: 60% left counts as low.
    app.cfg.usage.failover_pct = 60.0;
    app.failover_scan();
    assert!(app
        .failover_text(&app.failover[0])
        .starts_with("Account 3 has 50% left (weekly limit)"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_limit_message_in_the_tab_offers_a_move() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("output", &[]);
    let s = slot_of(&app, 0);
    app.failover_scan();
    assert!(app.failover.is_empty(), "the numbers are fine");
    app.panes[s].tabs[0].parser.lock().unwrap().process(
        b"working on it\r\n\r\nClaude usage limit reached. Your limit will reset at 1pm.\r\n",
    );
    app.failover_scan();
    assert_eq!(app.failover.len(), 1);
    assert!(app.failover[0].from_output);
    assert!(app
        .failover_text(&app.failover[0])
        .starts_with("Account 1 hit its usage limit."));
    assert!(crate::app_failover::limit_error(
        "You've hit your limit · resets 5pm"
    ));
    assert!(!crate::app_failover::limit_error("all tests passed\n> "));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn one_yes_moves_it_and_no_means_no_more_offers() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("yes", &[]);
    app.accounts[0].usage = Some(usage(100.0, 20.0));
    app.accounts[1].usage = Some(usage(0.0, 0.0));
    app.failover_scan();
    let uid = app.failover[0].uid;
    // Ctrl-a F: the yes.
    app.on_command(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('F'),
        crossterm::event::KeyModifiers::SHIFT,
    ));
    assert_eq!(app.pending_moves.len(), 1, "moved");
    assert_eq!(app.pending_moves[0].source_uid, uid);
    assert_eq!(app.pending_moves[0].target, 1);
    assert!(app.failover.iter().all(|o| o.uid != uid));
    // "Not now" on another one: not offered again for that account.
    let (mut app, home2) = test_app("no", &[]);
    app.accounts[0].usage = Some(usage(100.0, 20.0));
    app.failover_scan();
    assert_eq!(app.failover.len(), 1);
    app.decline_failover();
    app.failover_scan();
    assert!(app.failover.is_empty());
    assert!(app.pending_moves.is_empty());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&home2);
}

#[test]
fn auto_moves_only_idle_tabs_and_says_so() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("auto", &[]);
    app.cfg.usage.auto_failover = "auto".into();
    app.accounts[0].usage = Some(usage(100.0, 20.0));
    app.accounts[1].usage = Some(usage(100.0, 20.0));
    let (s0, s1) = (slot_of(&app, 0), slot_of(&app, 1));
    app.panes[s1].tabs[0].activity = Activity::Working;
    let busy = app.panes[s1].tabs[0].uid;
    app.failover_scan();
    assert_eq!(app.pending_moves.len(), 1, "the idle one moved");
    assert_eq!(app.pending_moves[0].source_uid, app.panes[s0].tabs[0].uid);
    assert!(app.flash.as_ref().unwrap().0.contains("so I moved"));
    // The busy one stays on offer, not moved.
    assert!(app.failover.iter().any(|o| o.uid == busy));
    assert!(app.queued_moves.is_empty());
    // Off: no offers at all.
    app.cfg.usage.auto_failover = "off".into();
    app.failover_scan();
    assert!(app.failover.is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn moves_stay_within_one_provider() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Accounts 3 and 4 are Grok.
    let (mut app, home) = test_app("provider", &[2, 3]);
    let grok = crate::harness::grok::parse_billing(&serde_json::json!({
        "creditUsagePercent": 100.0,
        "currentPeriod": {"start": "2026-10-02T14:16:00Z", "end": "2099-10-09T14:16:00Z"}
    }));
    app.accounts[2].usage = Some(grok);
    // The other Grok account is out too: no offer onto a Claude account.
    app.accounts[3].usage = app.accounts[2].usage.clone();
    app.failover_scan();
    assert!(app.failover.is_empty(), "{:?}", app.failover);
    // The other Grok account has room: offered there.
    app.accounts[3].usage = Some(crate::harness::grok::parse_billing(&serde_json::json!({
        "creditUsagePercent": 10.0,
        "currentPeriod": {"start": "2026-10-02T14:16:00Z", "end": "2099-10-09T14:16:00Z"}
    })));
    app.failover_scan();
    assert_eq!(app.failover.len(), 1);
    assert_eq!((app.failover[0].from, app.failover[0].to), (2, 3));
    let _ = std::fs::remove_dir_all(&home);
}
