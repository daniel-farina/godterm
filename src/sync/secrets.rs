//! A transcript secret scan: warning only (docs/SYNC.md 2.5). Built-in
//! patterns (the ones of section 1.5) over the bytes about to be pushed;
//! the count shows on the Sync screen. Matches are never shown or kept.

fn run_of(b: &[u8], i: usize, ok: impl Fn(u8) -> bool) -> usize {
    b[i..].iter().take_while(|&&c| ok(c)).count()
}

fn word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

/// How many likely secrets some bytes hold.
pub fn count(b: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < b.len() {
        let rest = &b[i..];
        let hit = if rest.starts_with(b"AKIA") {
            run_of(b, i + 4, |c| c.is_ascii_uppercase() || c.is_ascii_digit()) >= 16
        } else if rest.len() > 4
            && rest.starts_with(b"gh")
            && b"pousr".contains(&rest[2])
            && rest[3] == b'_'
        {
            run_of(b, i + 4, |c| c.is_ascii_alphanumeric()) >= 36
        } else if rest.starts_with(b"github_pat_") {
            run_of(b, i + 11, word) >= 80
        } else if rest.starts_with(b"sk-ant-") {
            run_of(b, i + 7, word) >= 20
        } else if rest.starts_with(b"xai-") {
            run_of(b, i + 4, |c| c.is_ascii_alphanumeric()) >= 20
        } else if rest.starts_with(b"AIza") {
            run_of(b, i + 4, word) >= 35
        } else if rest.len() > 5
            && rest.starts_with(b"xox")
            && b"abprs".contains(&rest[3])
            && rest[4] == b'-'
        {
            run_of(b, i + 5, word) >= 10
        } else if rest.starts_with(b"sk_live_") || rest.starts_with(b"rk_live_") {
            run_of(b, i + 8, |c| c.is_ascii_alphanumeric()) >= 20
        } else {
            rest.starts_with(b"-----BEGIN ") && {
                let line = &rest[..rest.len().min(80)];
                line.windows(16).any(|w| w == b"PRIVATE KEY-----")
            }
        };
        // Only at a word start, so "taskAKIA..." inside a word is not one.
        let start = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if hit && start {
            n += 1;
            i += 8;
        } else {
            i += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    #[test]
    fn patterns() {
        let k = format!("ghp_{}", "a".repeat(36));
        assert_eq!(super::count(k.as_bytes()), 1);
        assert_eq!(
            super::count(b"AKIAABCDEFGHIJKLMNOP and -----BEGIN RSA PRIVATE KEY-----"),
            2
        );
        assert_eq!(
            super::count(format!("sk-ant-{}", "x".repeat(30)).as_bytes()),
            1
        );
        assert_eq!(super::count(b"nothing here, sk-ant-short"), 0);
    }
}
