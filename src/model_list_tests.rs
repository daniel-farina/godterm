use crate::app_tabmodel::{index_of, models_for};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::harness::Harness;
use crate::model_list::*;

const ANTHROPIC: &str = r#"{"data":[
{"type":"model","id":"claude-opus-5-5","display_name":"Claude Opus 5.5","created_at":"2026-09-21T00:00:00Z","line":"opus","lifecycle":"active"},
{"type":"model","id":"claude-haiku-5-5","display_name":"Claude Haiku 5.5","created_at":"2026-10-07T18:00:00Z","line":"haiku","lifecycle":"active"},
{"type":"model","id":"claude-sonnet-5-5","display_name":"Claude Sonnet 5.5","created_at":"2026-09-28T00:00:00Z","line":"sonnet","lifecycle":"active"},
{"type":"model","id":"claude-fable-5-1","display_name":"Claude Fable 5.1","created_at":"2026-08-28T00:00:00Z","line":"fable","lifecycle":"active"},
{"type":"model","id":"claude-opus-5","display_name":"Claude Opus 5","created_at":"2026-07-24T00:00:00Z","line":"opus","lifecycle":"active"},
{"type":"model","id":"claude-old-1","display_name":"Claude Old 1","created_at":"2025-01-01T00:00:00Z","line":"old","lifecycle":"retired"}
],"has_more":false}"#;

const GROK: &str = "Default model: grok-4.5\n\nAvailable models:\n  - grok-4.7\n  - grok-4.7-build-fast\n  - grok-4.6\n  * grok-4.5 (default)\n  - gemma-4-31b\n";

const GROK_ANON: &str = "You are not authenticated.\n\nDefault model: grok-4.6\n\nAvailable models:\n  * grok-4.6 (default)\n  - grok-4.5\n";

const CLAUDE_MENU: &str = "Select model\nSwitch between Claude models. Your pick becomes the default for new sessions.\n\n   1. Default (recommended)  Sonnet 5.5 · Efficient for routine tasks\n   2. Opus 5.5               For complex work and everyday tasks\n   3. Fable 5.1              For your toughest challenges\n ❯ 4. Sonnet 5.5 ✔           Most efficient for simpler tasks\n   5. Haiku 5.5              Fastest for quick answers\n   6. Haiku 4.5              Fastest for quick answers\n\nEnter to confirm · Esc to exit\n";

fn ids(v: &[Model]) -> Vec<&str> {
    v.iter().map(|m| m.id.as_str()).collect()
}

#[test]
fn anthropic_reply_is_newest_first_without_retired_ones() {
    let v = parse_anthropic(ANTHROPIC);
    assert_eq!(
        ids(&v),
        [
            "claude-haiku-5-5",
            "claude-sonnet-5-5",
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-opus-5"
        ]
    );
    assert_eq!(v[0].name, "Haiku 5.5");
    assert_eq!(v[0].line, "haiku");
    assert!(parse_anthropic("not json").is_empty());
    assert!(parse_anthropic(r#"{"error":{"type":"authentication_error"}}"#).is_empty());
}

#[test]
fn grok_models_lists_the_menu_and_refuses_the_anonymous_stub() {
    let v = parse_grok_models(GROK);
    assert_eq!(
        ids(&v),
        [
            "grok-4.7",
            "grok-4.7-build-fast",
            "grok-4.6",
            "grok-4.5",
            "gemma-4-31b"
        ]
    );
    assert!(parse_grok_models(GROK_ANON).is_empty());
    assert!(parse_grok_models("").is_empty());
}

#[test]
fn claude_menu_rows_become_models() {
    let v = parse_claude_menu(CLAUDE_MENU);
    assert_eq!(
        ids(&v),
        [
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-haiku-4-5"
        ]
    );
    assert_eq!(v[2].name, "Sonnet 5.5");

    assert!(parse_claude_menu("1. Yes\n2. No, exit\n3. Do not ask again").is_empty());
}

#[test]
fn grok_menu_words_are_models_once_each() {
    let screen = "Select model\n > grok-4.7  most capable\n   grok-4.7-build-fast  quick\n   grok-4.6\nused grok-4.7 earlier, see grok-docs";
    assert_eq!(
        ids(&parse_grok_menu(screen)),
        ["grok-4.7", "grok-4.7-build-fast", "grok-4.6"]
    );
}

#[test]
fn claude_bar_has_the_newest_of_each_family_in_order() {
    let bar = claude_bar(&parse_anthropic(ANTHROPIC));
    let args: Vec<&str> = bar.iter().map(|b| b.arg.as_str()).collect();
    assert_eq!(args, ["haiku", "sonnet", "opus", "claude-fable-5-1"]);
    assert_eq!(bar[2].label, "Opus 5.5");
    assert_eq!(bar[2].desc, "thorough");

    assert_eq!(index_of(&bar, "claude-opus-5-5"), Some(2));
    assert_eq!(index_of(&bar, "Fable 5.1"), Some(3));
}

#[test]
fn grok_bar_keeps_listed_order_and_labels() {
    let bar = grok_bar(&parse_grok_models(GROK));
    let args: Vec<&str> = bar.iter().map(|b| b.arg.as_str()).collect();
    assert_eq!(
        args,
        ["grok-4.7", "grok-4.7-build-fast", "grok-4.6", "grok-4.5"]
    );
    assert_eq!(bar[1].label, "4.7 fast");
    assert_eq!(bar[1].desc, "quickest");
    assert_eq!(bar[0].desc, "most capable");
    assert_eq!(index_of(&bar, "grok-4.7-build-fast"), Some(1));
    assert_eq!(index_of(&bar, "grok-4.7"), Some(0));
}

#[test]
fn assistant_ids_one_per_family() {
    let v = parse_anthropic(ANTHROPIC);
    assert_eq!(
        assistant_ids("claude", &v),
        [
            "claude-haiku-5-5",
            "claude-sonnet-5-5",
            "claude-opus-5-5",
            "claude-fable-5-1"
        ]
    );
    assert_eq!(
        assistant_ids("grok", &parse_grok_models(GROK)),
        ["grok-4.7", "grok-4.7-build-fast", "grok-4.6", "grok-4.5"]
    );
}

#[test]
fn bar_uses_the_cache_then_the_override_wins() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    testing::clear();
    let mut cfg = crate::config::Config::default();

    let reg = models_for(Harness::Claude, &cfg);
    assert_eq!(reg[0].arg, "haiku");
    assert!(bar(Harness::Claude, &cfg).is_none());
    testing::set("claude", parse_anthropic(ANTHROPIC));
    let live = models_for(Harness::Claude, &cfg);
    assert_eq!(live[2].label, "Opus 5.5");
    assert_eq!(
        assistant_models("claude", &cfg).unwrap()[1],
        "claude-sonnet-5-5"
    );

    cfg.tab_models.claude = vec!["opus".into(), "mystery=Mine".into()];
    let over = models_for(Harness::Claude, &cfg);
    assert_eq!(over.len(), 2);
    assert_eq!(over[0].label, "Opus 5.5");
    assert_eq!(over[1].label, "Mine");

    assert!(models_for(Harness::Codex, &cfg).is_empty());
    testing::clear();
}
