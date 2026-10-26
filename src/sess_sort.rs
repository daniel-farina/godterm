//! Sorting, grouping and the headless filter for session lists, shared by
//! the Sessions view and the assistant's `sessions` tool.

use std::cmp::Ordering;

use crate::sessions::SessionInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Modified,
    Created,
    Messages,
    Tokens,
    Context,
    Project,
    Source,
    Title,
}

pub const KEYS: [SortKey; 8] = [
    SortKey::Modified,
    SortKey::Created,
    SortKey::Messages,
    SortKey::Tokens,
    SortKey::Context,
    SortKey::Project,
    SortKey::Source,
    SortKey::Title,
];

impl SortKey {
    /// The name used in state.json and the tool ("modified", "context", ...).
    pub fn name(self) -> &'static str {
        match self {
            SortKey::Modified => "modified",
            SortKey::Created => "created",
            SortKey::Messages => "messages",
            SortKey::Tokens => "tokens",
            SortKey::Context => "context",
            SortKey::Project => "project",
            SortKey::Source => "source",
            SortKey::Title => "title",
        }
    }

    /// The menu label.
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Modified => "Modified",
            SortKey::Created => "Created",
            SortKey::Messages => "Messages",
            SortKey::Tokens => "Tokens",
            SortKey::Context => "Largest context",
            SortKey::Project => "Project",
            SortKey::Source => "Source",
            SortKey::Title => "Title",
        }
    }

    pub fn parse(s: &str) -> Option<SortKey> {
        let s = s.trim().to_ascii_lowercase();
        let s = s.as_str();
        Some(match s {
            "modified" | "newest" | "recent" | "updated" | "date" => SortKey::Modified,
            "created" | "started" => SortKey::Created,
            "messages" | "msgs" => SortKey::Messages,
            "tokens" => SortKey::Tokens,
            "context" | "largest context" | "largest_context" => SortKey::Context,
            "project" | "folder" | "cwd" => SortKey::Project,
            "source" | "account" => SortKey::Source,
            "title" | "name" => SortKey::Title,
            _ => return None,
        })
    }

    pub fn next(self) -> SortKey {
        let i = KEYS.iter().position(|k| *k == self).unwrap_or(0);
        KEYS[(i + 1) % KEYS.len()]
    }

    /// Numbers and times sort biggest / newest first; text A to Z.
    pub fn descending(self) -> bool {
        !matches!(self, SortKey::Project | SortKey::Source | SortKey::Title)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Group {
    #[default]
    None,
    Source,
    Project,
    Harness,
}

pub const GROUPS: [Group; 4] = [Group::None, Group::Source, Group::Project, Group::Harness];

impl Group {
    pub fn name(self) -> &'static str {
        match self {
            Group::None => "none",
            Group::Source => "source",
            Group::Project => "project",
            Group::Harness => "harness",
        }
    }

    pub fn parse(s: &str) -> Option<Group> {
        GROUPS
            .iter()
            .copied()
            .find(|g| g.name() == s.trim().to_ascii_lowercase())
    }
}

/// Compare two sessions by `key` in its natural direction; `rev` flips it.
/// Ties fall back to newest first.
pub fn compare(
    key: SortKey,
    rev: bool,
    a: (&str, &SessionInfo),
    b: (&str, &SessionInfo),
) -> Ordering {
    let (sa, ia) = a;
    let (sb, ib) = b;
    let o = match key {
        SortKey::Modified => ia.modified.cmp(&ib.modified),
        SortKey::Created => ia.created.or(ia.modified).cmp(&ib.created.or(ib.modified)),
        SortKey::Messages => ia.messages.cmp(&ib.messages),
        SortKey::Tokens => ia.tokens.total().cmp(&ib.tokens.total()),
        SortKey::Context => ia.tokens.context.cmp(&ib.tokens.context),
        SortKey::Project => ia.cwd.to_lowercase().cmp(&ib.cwd.to_lowercase()),
        SortKey::Source => sa.to_lowercase().cmp(&sb.to_lowercase()),
        SortKey::Title => ia
            .summary()
            .to_lowercase()
            .cmp(&ib.summary().to_lowercase()),
    };
    let o = if key.descending() { o.reverse() } else { o };
    let o = if rev { o.reverse() } else { o };
    o.then_with(|| ib.modified.cmp(&ia.modified))
}

/// A folder only scripts and tools use: the temp dirs. The demo's own
/// world counts as a home, not a temp dir, wherever it was made (a demo
/// started with HOME in a temp dir would hide every session it seeded).
pub fn temp_dir(cwd: &str) -> bool {
    if crate::demo::active() {
        let home = crate::demo::home();
        let real = std::fs::canonicalize(&home).unwrap_or_else(|_| home.clone());
        for h in [home, real] {
            let h = h.to_string_lossy();
            let h = h.trim_end_matches('/');
            if !h.is_empty() && (cwd == h || cwd.starts_with(&format!("{h}/"))) {
                return false;
            }
        }
    }
    let tmp = std::env::var("TMPDIR").unwrap_or_default();
    let tmp = tmp.trim_end_matches('/');
    let under = |p: &str| !p.is_empty() && (cwd == p || cwd.starts_with(&format!("{p}/")));
    under("/tmp")
        || under("/private/tmp")
        || under("/private/var/folders")
        || under("/var/folders")
        || under(tmp)
        || (!tmp.is_empty() && under(&format!("/private{tmp}")))
}

/// Not something a person ran in a terminal: a non-interactive run
/// (claude -p / the SDK, grok headless) or one in a temp folder.
pub fn headless(s: &SessionInfo) -> bool {
    s.headless || temp_dir(&s.cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn s(
        id: &str,
        cwd: &str,
        mins_ago: u64,
        msgs: usize,
        tokens: u64,
        ctx: u64,
        title: &str,
    ) -> SessionInfo {
        let mut x = SessionInfo {
            id: id.into(),
            cwd: cwd.into(),
            messages: msgs,
            title: Some(title.into()),
            ..Default::default()
        };
        x.modified = Some(SystemTime::now() - Duration::from_secs(mins_ago * 60));
        x.created =
            Some(SystemTime::now() - Duration::from_secs((mins_ago + 100 - msgs as u64) * 60));
        x.tokens.output = tokens;
        x.tokens.context = ctx;
        x
    }

    #[test]
    fn every_sort_and_reverse() {
        let v = [
            ("Acct 2", s("a", "/w/zeta", 5, 10, 300, 9, "beta")),
            ("Acct 1", s("b", "/w/alpha", 1, 30, 100, 50, "alpha")),
            ("main", s("c", "/w/mid", 9, 20, 200, 20, "Gamma")),
        ];
        let order = |k: SortKey, rev: bool| {
            let mut r: Vec<&(&str, SessionInfo)> = v.iter().collect();
            r.sort_by(|a, b| compare(k, rev, (a.0, &a.1), (b.0, &b.1)));
            r.iter().map(|x| x.1.id.as_str()).collect::<String>()
        };
        assert_eq!(order(SortKey::Modified, false), "bac");
        assert_eq!(order(SortKey::Modified, true), "cab");
        // Created: b (30 msgs) is 71+1 min old, a 90+5, c 80+9.
        assert_eq!(order(SortKey::Created, false), "bca");
        assert_eq!(order(SortKey::Messages, false), "bca");
        assert_eq!(order(SortKey::Tokens, false), "acb");
        assert_eq!(order(SortKey::Context, false), "bca");
        assert_eq!(order(SortKey::Project, false), "bca");
        assert_eq!(order(SortKey::Project, true), "acb");
        assert_eq!(order(SortKey::Source, false), "bac");
        assert_eq!(order(SortKey::Title, false), "bac");
        for k in KEYS {
            assert_eq!(SortKey::parse(k.name()), Some(k));
        }
        assert_eq!(SortKey::Title.next(), SortKey::Modified);
        assert_eq!(Group::parse("project"), Some(Group::Project));
    }

    #[test]
    fn headless_by_kind_or_temp_folder() {
        let mut x = s("a", "/Users/me/code", 1, 1, 1, 1, "t");
        assert!(!headless(&x));
        x.headless = true;
        assert!(headless(&x));
        for cwd in [
            "/tmp/x",
            "/private/tmp",
            "/private/var/folders/mw/abc/T/sub8-claude-x",
        ] {
            x = s("a", cwd, 1, 1, 1, 1, "t");
            assert!(headless(&x), "{cwd}");
        }
        assert!(!temp_dir("/tmpfoo"));
    }

    #[test]
    fn the_demo_world_is_not_a_temp_dir() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = "/private/tmp/godterm-demo-sort-test/.godterm-demo";
        let old = (
            std::env::var(crate::demo::ENV).ok(),
            std::env::var("GODTERM_DEMO_HOME").ok(),
        );
        std::env::set_var("GODTERM_DEMO_HOME", home);
        std::env::set_var(crate::demo::ENV, "1");
        let demo = temp_dir(&format!("{home}/code/payments-api"));
        let other = temp_dir("/private/tmp/scratch/run");
        std::env::remove_var(crate::demo::ENV);
        let off = temp_dir(&format!("{home}/code/payments-api"));
        for (k, v) in [(crate::demo::ENV, old.0), ("GODTERM_DEMO_HOME", old.1)] {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        assert!(!demo, "the demo's own sessions show");
        assert!(other, "other temp folders stay hidden in the demo");
        assert!(off, "outside the demo a temp folder is a temp folder");
    }
}
