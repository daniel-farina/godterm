use super::*;
use crate::git_ops::tests::{fixture, g};

fn samples() -> Vec<(&'static str, String)> {
    let r = |c: char, n: usize| c.to_string().repeat(n);
    vec![
        ("aws-access-key", format!("AKIA{}", r('Q', 16))),
        ("github-token", format!("ghp_{}", r('a', 36))),
        ("github-pat", format!("github_pat_{}", r('b', 82))),
        ("anthropic-key", format!("sk-ant-{}", r('c', 24))),
        ("openai-key", format!("sk-proj-{}", r('d', 24))),
        ("xai-key", format!("xai-{}", r('e', 24))),
        ("google-api-key", format!("AIza{}", r('f', 35))),
        ("slack-token", format!("xoxb-{}", r('1', 12))),
        ("stripe-live-key", format!("sk_live_{}", r('g', 24))),
        ("private-key", "-----BEGIN RSA PRIVATE KEY-----".to_string()),
        ("generic-secret", format!("api_key = \"{}\"", r('h', 20))),
    ]
}

#[test]
fn every_pattern_hits_and_is_redacted() {
    for (rule, secret) in samples() {
        let text = format!("let x = 1;\nconst K = {secret};\n");
        let f = scan_text("src/a.rs", &text, 1);
        assert!(!f.is_empty(), "{rule} missed");
        assert_eq!(f[0].rule, rule, "{secret}");
        assert_eq!(f[0].line, 2);
        assert!(!f[0].redacted.contains(&secret), "{rule} shows the value");
        assert!(!f[0].line_text().contains(&secret));
        let red = redact_text(&text);
        if rule != "private-key" {
            assert!(!red.contains(&secret), "{rule}: {red}");
        }
    }
    assert!(scan_text("a", "plain words, tokenizer = 3\n", 1).is_empty());
    assert_eq!(redact("short"), "*****");
    assert!(redact(&"z".repeat(40)).starts_with("zzzz****(40 chars)"));
}

#[test]
fn patch_lines_and_files() {
    let key = format!("ghp_{}", "Z".repeat(36));
    let patch = format!(
        "diff --git a/x b/x\n--- a/x\n+++ b/conf/x.env.rb\n@@ -1,0 +5,2 @@\n+ok\n+token: {key}\n-gone {key}\n"
    );
    let f = scan_patch(&patch);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!((f[0].file.as_str(), f[0].line), ("conf/x.env.rb", 6));
}

#[test]
fn gitleaks_report_fixture() {
    let json = r#"[{"RuleID":"github-pat","File":"app/config.js","StartLine":12,
        "Secret":"REDACTED","Match":"token: REDACTED","Commit":"abc"},
        {"RuleID":"generic-api-key","File":"x.py","StartLine":3,"Secret":"abcd1234abcd1234abcd"}]"#;
    let f = parse_gitleaks(json).unwrap();
    assert_eq!(f.len(), 2);
    assert_eq!(f[0].rule, "github-pat");
    assert_eq!(f[0].redacted, "REDACTED");
    assert_eq!(f[1].redacted, "abcd****(20 chars)");
    assert!(parse_gitleaks("").unwrap().is_empty());
    assert!(parse_gitleaks("not json").is_err());
    let a = gitleaks_args(Path::new("/p"), Some("origin/main..HEAD"));
    for want in [
        "git",
        "/p",
        "--redact",
        "--no-banner",
        "--report-path",
        "-",
        "--log-opts=origin/main..HEAD",
    ] {
        assert!(a.contains(&want.to_string()), "{a:?}");
    }
}

#[test]
fn builtin_scan_of_outgoing_commits() {
    let fx = fixture("scan");
    let ctx = Ctx::new(&fx.a);

    assert!(gitleaks_bin().is_none());
    assert!(scan_outgoing(&ctx, None).findings.is_empty());
    let key = format!("AKIA{}", "W".repeat(16));
    std::fs::write(fx.a.join("cfg.txt"), format!("aws = {key}\n")).unwrap();
    g(&fx.a, &["add", "cfg.txt"]);
    g(&fx.a, &["commit", "-q", "-m", "cfg"]);
    let s = scan_outgoing(&ctx, None);
    assert_eq!(s.tool, "built-in");
    assert_eq!(s.findings.len(), 1, "{s:?}");
    assert_eq!(s.findings[0].file, "cfg.txt");
    assert!(!format!("{s:?}").contains(&key));
    let c = check_push(&ctx);
    assert_eq!(c.soft_blocks().len(), 1);
    assert!(c.hard_blocks().is_empty(), "{:?}", c.hard_blocks());

    std::fs::write(
        fx.a.join(".gitleaksignore"),
        "abc123:cfg.txt:aws-access-key:1\n",
    )
    .unwrap();
    assert!(scan_outgoing(&ctx, None).findings.is_empty());
    let _ = std::fs::remove_dir_all(&fx.root);
}

fn sparse(p: &Path, n: u64) {
    let f = std::fs::File::create(p).unwrap();
    f.set_len(n).unwrap();
}

#[test]
fn size_thresholds() {
    const MB: u64 = 1024 * 1024;
    let s = classify(&[
        ("a".into(), 10 * MB),
        ("b".into(), 51 * MB),
        ("c".into(), 96 * MB),
        ("d".into(), 50 * MB),
        ("e".into(), 95 * MB),
    ]);
    assert_eq!(s.warn, vec![("b".into(), 51 * MB), ("e".into(), 95 * MB)]);
    assert_eq!(s.block, vec![("c".into(), 96 * MB)]);
    assert!(s.too_big());
    let many: Vec<(String, u64)> = (0..17).map(|i| (format!("f{i}"), 94 * MB)).collect();
    assert!(classify(&many).total > TOTAL_BYTES && classify(&many).too_big());

    let fx = fixture("size");
    let ctx = Ctx::new(&fx.a);
    sparse(&fx.a.join("mid.bin"), 51 * MB);
    sparse(&fx.a.join("huge.bin"), 96 * MB);
    let wt = big_worktree_files(
        &ctx,
        &["mid.bin".into(), "huge.bin".into(), "README.md".into()],
    );
    assert_eq!(wt.len(), 2);
    g(&fx.a, &["add", "mid.bin", "huge.bin"]);
    g(&fx.a, &["commit", "-q", "-m", "big"]);
    let s = outgoing_sizes(&ctx).unwrap();
    assert_eq!(s.warn.len(), 1, "{s:?}");
    assert_eq!(s.block.len(), 1, "{s:?}");
    assert_eq!(s.block[0].0, "huge.bin");
    let c = check_push(&ctx);
    assert!(c.hard_blocks().iter().any(|b| b.contains("huge.bin")));
    assert!(c.warnings().iter().any(|w| w.contains("mid.bin")));
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn secret_files_links_and_ignores() {
    let fx = fixture("hyg");
    let ctx = Ctx::new(&fx.a);
    for f in [".env", ".env.example", "deploy.pem", "notes.md"] {
        std::fs::write(fx.a.join(f), "x\n").unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("/Volumes/Private/stuff", fx.a.join("vault-link")).unwrap();
    g(&fx.a, &["add", "-A"]);
    let mut s = tracked_secret_files(&ctx);
    s.sort();
    assert_eq!(s, vec![".env".to_string(), "deploy.pem".to_string()]);
    #[cfg(unix)]
    {
        let l = bad_symlinks(&ctx);
        assert_eq!(l.len(), 1);
        assert!(l[0].1.contains("vault"));
        assert!(!check_push(&ctx).hard_blocks().is_empty());
    }
    assert!(env_file(".env.local") && !env_file(".env.example") && !env_file("env.rs"));

    let miss = missing_ignores(&ctx);
    assert!(miss.contains(&".env".to_string()) && miss.contains(&"node_modules/".to_string()));
    add_ignores(&fx.a, &miss).unwrap();
    assert!(
        missing_ignores(&ctx).is_empty(),
        "{:?}",
        missing_ignores(&ctx)
    );
    let gi = std::fs::read_to_string(fx.a.join(".gitignore")).unwrap();
    assert!(gi.contains("!.env.example"));
    assert!(add_ignores(&fx.a, &["*".to_string()]).is_err());

    crate::git_ops::untrack(&ctx, &s).unwrap();
    assert!(tracked_secret_files(&ctx).is_empty());
    assert!(fx.a.join(".env").exists());
    let _ = std::fs::remove_dir_all(&fx.root);
}
