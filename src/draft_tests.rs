use crate::pane::typed_draft;

fn screen(bytes: &str) -> vt100::Parser {
    let mut p = vt100::Parser::new(12, 60, 0);
    p.process(bytes.as_bytes());
    p
}

#[test]
fn typed_text_in_the_input_box_is_a_draft() {
    let p = screen("old output\r\n\x1b[0m❯ half a message\r\n\r\n  bypass permissions on");
    assert!(typed_draft(p.screen()));
}

#[test]
fn a_dim_suggestion_is_not_a_draft() {
    let p = screen("old output\r\n❯ \x1b[2mcommit this\x1b[0m\r\n\r\n  bypass permissions on");
    assert!(!typed_draft(p.screen()));
}

#[test]
fn a_grey_suggestion_is_not_a_draft() {
    let p = screen("old output\r\n❯ \x1b[90mrun the tests\x1b[0m\r\n  bypass permissions on");
    assert!(!typed_draft(p.screen()));
}

#[test]
fn an_empty_input_box_is_not_a_draft() {
    let p = screen("❯ earlier prompt\r\nanswer\r\n❯ \r\n  bypass permissions on");
    assert!(!typed_draft(p.screen()));
}

#[test]
fn claude_tabs_never_report_errors_or_telemetry() {
    let env = crate::providers::claude::no_reporting_env(crate::harness::Harness::Claude);
    for k in [
        "DISABLE_TELEMETRY",
        "DISABLE_ERROR_REPORTING",
        "DO_NOT_TRACK",
    ] {
        assert!(env.iter().any(|(n, v)| n == k && v == "1"), "{k}");
    }
    let grok = crate::providers::claude::no_reporting_env(crate::harness::Harness::Grok);
    assert!(grok.iter().any(|(n, _)| n == "DO_NOT_TRACK"));
    assert!(!grok.iter().any(|(n, _)| n == "DISABLE_ERROR_REPORTING"));
}
