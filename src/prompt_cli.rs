use crate::harness::Harness;
use crate::pane::Activity;
use crate::prompt::{Prompt, PromptKind, PromptOption, Request};

fn lines(screen: &str) -> Vec<&str> {
    screen.lines().map(str::trim_end).collect()
}

fn column_label(s: &str) -> &str {
    match s.find("  ") {
        Some(i) => s[..i].trim_end(),
        None => s.trim_end(),
    }
}

fn split_hotkey(label: &str) -> (String, Option<Vec<u8>>) {
    let t = label.trim_end();
    if let Some(open) = t.rfind(" (") {
        if let Some(inner) = t[open + 2..].strip_suffix(')') {
            let key: Option<Vec<u8>> = match inner {
                "esc" => Some(vec![0x1b]),
                "tab" => Some(vec![b'\t']),
                "shift+tab" => Some(b"\x1b[Z".to_vec()),
                "esc or n" => Some(b"n".to_vec()),
                s if s.chars().count() == 1 && s.chars().all(|c| c.is_ascii_alphanumeric()) => {
                    Some(s.as_bytes().to_vec())
                }
                _ => None,
            };
            if key.is_some() {
                return (t[..open].trim_end().to_string(), key);
            }
        }
    }
    (t.to_string(), None)
}

fn numbered(line: &str) -> Option<(bool, u32, String)> {
    let t = line.trim_start();
    let (sel, rest) = match t.strip_prefix('›').or_else(|| t.strip_prefix('>')) {
        Some(r) => (true, r.trim_start()),
        None => (false, t),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let label = rest[digits.len()..].strip_prefix(". ")?;
    Some((sel, digits.parse().ok()?, label.trim().to_string()))
}

#[allow(clippy::type_complexity)]
fn last_numbered_block(ls: &[&str]) -> Option<(usize, Vec<(bool, u32, String)>)> {
    let col = |l: &str| {
        l.chars()
            .take_while(|c| *c == ' ' || *c == '›' || *c == '>')
            .count()
    };
    let indent = |l: &str| l.chars().take_while(|c| *c == ' ').count();
    let mut best: Option<(usize, Vec<(bool, u32, String)>)> = None;
    let mut cur: Option<(usize, Vec<(bool, u32, String)>, usize)> = None;
    for (i, l) in ls.iter().enumerate() {
        if let Some(o) = numbered(l) {
            let c = col(l);
            match &mut cur {
                Some((_, v, last_col)) => {
                    v.push(o);
                    *last_col = c;
                }
                None => cur = Some((i, vec![o], c)),
            }
            continue;
        }
        if let Some((_, v, c)) = &mut cur {
            if !l.trim().is_empty() && indent(l) > *c {
                if let Some(last) = v.last_mut() {
                    last.2 = format!("{} {}", last.2, l.trim());
                }
                continue;
            }
        }
        if let Some((start, v, _)) = cur.take() {
            if v.len() >= 2 {
                best = Some((start, v));
            }
        }
    }
    if let Some((start, v, _)) = cur.take() {
        if v.len() >= 2 {
            best = Some((start, v));
        }
    }
    best
}

type OptKeys = (String, Option<u32>, Option<Vec<u8>>);

fn finish(
    kind: PromptKind,
    opts: Vec<(String, Option<u32>, Option<Vec<u8>>)>,
    selected: usize,
    pick: [Option<usize>; 3],
) -> Prompt {
    Prompt {
        kind,
        selected,
        hotkeys: opts.iter().map(|o| o.2.clone()).collect(),
        options: opts
            .into_iter()
            .map(|(label, number, _)| PromptOption { label, number })
            .collect(),
        pick: Some(pick),
    }
}

fn pick_by_label(labels: &[String]) -> [Option<usize>; 3] {
    let low: Vec<String> = labels.iter().map(|l| l.to_lowercase()).collect();
    let find = |f: &dyn Fn(&str) -> bool| low.iter().position(|l| f(l));
    let always = |l: &str| {
        l.contains("don't ask again") || l.starts_with("always allow") || l.contains("always")
    };
    [
        find(&|l| {
            (l.starts_with("yes") && !always(l)) || l == "allow" || l.starts_with("allow once")
        }),
        find(&|l| always(l)).or_else(|| find(&|l| l.starts_with("allow for this session"))),
        find(&|l| l.starts_with("no") || l == "cancel" || l.starts_with("reject")),
    ]
}

fn codex_prompt(screen: &str) -> Option<Prompt> {
    let ls = lines(screen);
    let (start, block) = last_numbered_block(&ls)?;
    let above = ls[start.saturating_sub(14)..start].join("\n");
    let kind = if above.contains("trust the contents") {
        PromptKind::Trust
    } else if above.contains("Would you like to")
        || above.contains("Do you want to")
        || (above.contains("Allow the") && above.contains("to run tool"))
    {
        PromptKind::Permission
    } else {
        PromptKind::Other
    };
    let selected = block.iter().position(|b| b.0).unwrap_or(0);
    let opts: Vec<_> = block
        .iter()
        .map(|(_, n, l)| {
            let (label, key) = split_hotkey(column_label(l));
            (label, Some(*n), key)
        })
        .collect();
    let labels: Vec<String> = opts.iter().map(|o| o.0.clone()).collect();
    let pick = pick_by_label(&labels);
    Some(finish(kind, opts, selected, pick))
}

fn cursor_prompt(screen: &str) -> Option<Prompt> {
    let ls = lines(screen);
    if screen.contains("Workspace Trust Required") {
        let mut opts = vec![];
        let mut selected = 0;
        for l in &ls {
            let t = l.trim().trim_matches('│').trim();
            let (sel, t) = match t.strip_prefix('▶') {
                Some(r) => (true, r.trim_start()),
                None => (false, t),
            };
            if let Some(rest) = t.strip_prefix('[') {
                if let Some((k, label)) = rest.split_once("] ") {
                    if k.chars().count() == 1 {
                        if sel {
                            selected = opts.len();
                        }
                        opts.push((label.trim().to_string(), None, Some(k.as_bytes().to_vec())));
                    }
                }
            }
        }
        if opts.len() >= 2 {
            return Some(finish(
                PromptKind::Trust,
                opts,
                selected,
                [Some(0), None, Some(1)],
            ));
        }
        return None;
    }

    let q = ls.iter().rposition(|l| {
        let t = l.trim();
        t.starts_with("Run this") && t.ends_with('?')
    })?;

    let known = |label: &str| -> Option<Vec<u8>> {
        let l = label.to_lowercase();
        if l.starts_with("run (once)") {
            Some(b"y".to_vec())
        } else if l.starts_with("run everything") {
            Some(b"\x1b[Z".to_vec())
        } else if l.starts_with("add ") || l.starts_with("allowlist") {
            Some(b"\t".to_vec())
        } else if l.starts_with("skip") {
            Some(b"n".to_vec())
        } else {
            None
        }
    };
    let mut opts = vec![];
    let mut selected = 0;
    let mut pending: Option<(bool, String)> = None;
    let flush = |p: &mut Option<(bool, String)>, opts: &mut Vec<OptKeys>, selected: &mut usize| {
        if let Some((sel, body)) = p.take() {
            if let Some(k) = known(&body) {
                if sel {
                    *selected = opts.len();
                }
                opts.push((body, None, Some(k)));
            }
        }
    };
    for l in &ls[q + 1..] {
        let t = l.trim_end();
        let body = t.trim_start();
        let (sel, body) = match body.strip_prefix('→') {
            Some(r) => (true, r.trim_start()),
            None => (false, body),
        };
        if let Some((psel, pbody)) = pending.take() {
            if !sel && !body.is_empty() && known(body).is_none() {
                let joined = format!("{pbody} {body}");
                let (label, key) = split_hotkey(&joined);
                if key.is_some() {
                    if psel {
                        selected = opts.len();
                    }
                    opts.push((label, None, key));
                } else {
                    pending = Some((psel, joined));
                }
                continue;
            }
            pending = Some((psel, pbody));
            flush(&mut pending, &mut opts, &mut selected);
        }
        let (label, key) = split_hotkey(body);
        if key.is_some() {
            if sel {
                selected = opts.len();
            }
            opts.push((label, None, key));
        } else if known(body).is_some() {
            pending = Some((sel, body.to_string()));
        }
    }
    flush(&mut pending, &mut opts, &mut selected);
    if opts.len() < 2 {
        return None;
    }
    let low: Vec<String> = opts.iter().map(|o| o.0.to_lowercase()).collect();
    let find = |f: &dyn Fn(&str) -> bool| low.iter().position(|l| f(l));
    let pick = [
        find(&|l| l.starts_with("run (once)")),
        find(&|l| l.starts_with("add ") || l.starts_with("allowlist")),
        find(&|l| l.starts_with("skip")).or_else(|| find(&|l| l.starts_with("reject"))),
    ];
    Some(finish(PromptKind::Permission, opts, selected, pick))
}

fn antigravity_prompt(screen: &str) -> Option<Prompt> {
    let ls = lines(screen);
    if screen.contains("Do you trust the contents") {
        let q = ls
            .iter()
            .position(|l| l.contains("Do you trust the contents"))?;
        let rest = &ls[q + 1..];
        let opt_line = |l: &str| !l.trim().is_empty() && !l.contains("Navigate");
        let hi = rest.iter().position(|l| l.starts_with("> "))?;
        let mut start = hi;
        while start > 0 && opt_line(rest[start - 1]) {
            start -= 1;
        }
        let mut end = hi + 1;
        while end < rest.len() && opt_line(rest[end]) {
            end += 1;
        }
        let mut opts = vec![];
        let mut selected = 0;
        for (i, l) in rest[start..end].iter().enumerate() {
            if start + i == hi {
                selected = opts.len();
            }
            let label = l.strip_prefix("> ").unwrap_or(l).trim();
            opts.push((label.to_string(), None, None));
        }
        if opts.len() >= 2 {
            let labels: Vec<String> = opts.iter().map(|o| o.0.clone()).collect();
            let pick = pick_by_label(&labels);
            return Some(finish(PromptKind::Trust, opts, selected, pick));
        }
        return None;
    }
    if !screen.contains("Requesting permission for") && !screen.contains("Run this") {
        return None;
    }
    let (_, block) = last_numbered_block(&ls)?;
    let selected = block.iter().position(|b| b.0).unwrap_or(0);
    let opts: Vec<_> = block
        .iter()
        .map(|(_, n, l)| (l.clone(), Some(*n), None))
        .collect();
    let labels: Vec<String> = opts.iter().map(|o| o.0.clone()).collect();
    let pick = pick_by_label(&labels);
    Some(finish(PromptKind::Permission, opts, selected, pick))
}

pub fn parse_prompt_for(h: Harness, screen: &str) -> Option<Prompt> {
    match h {
        Harness::Codex => codex_prompt(screen),
        Harness::Cursor => cursor_prompt(screen),
        Harness::Antigravity => antigravity_prompt(screen),
        _ => crate::prompt::parse_prompt(screen),
    }
}

pub fn describe_request_for(h: Harness, screen: &str) -> Option<Request> {
    if !matches!(h, Harness::Codex | Harness::Cursor | Harness::Antigravity) {
        return crate::prompt::describe_request(screen);
    }
    let p = parse_prompt_for(h, screen)?;
    let ls = lines(screen);
    let clean = |s: &str| s.trim().to_string();
    if p.kind == PromptKind::Trust {
        let after = |marker: &str| {
            ls.iter().position(|l| l.contains(marker)).and_then(|i| {
                ls[i + 1..]
                    .iter()
                    .map(|l| l.trim_matches(|c: char| c == '│' || c.is_whitespace()))
                    .find(|l| !l.is_empty())
            })
        };
        let detail = ls
            .iter()
            .find_map(|l| l.trim().strip_prefix("> You are in "))
            .or_else(|| after("Accessing workspace"))
            .or_else(|| after("Do you trust the contents of this directory"))
            .unwrap_or("")
            .trim_matches('│')
            .trim()
            .to_string();
        return Some(Request {
            tool: "Trust folder".into(),
            detail,
            question: "trust this folder?".into(),
        });
    }
    if p.kind != PromptKind::Permission {
        return None;
    }
    match h {
        Harness::Codex => {
            let q = ls.iter().rposition(|l| {
                l.contains("Would you like to")
                    || l.contains("Do you want to")
                    || l.contains("Allow the")
            })?;
            let question = clean(ls[q]);
            if question.contains("MCP server") {
                let tool = question.split('"').nth(1).unwrap_or("").to_string();
                let server = question
                    .strip_prefix("Allow the ")
                    .and_then(|s| s.split(" MCP").next())
                    .unwrap_or("");
                return Some(Request {
                    tool: "MCP tool".into(),
                    detail: format!("{server} {tool}").trim().to_string(),
                    question,
                });
            }
            let detail = ls[q..]
                .iter()
                .find_map(|l| l.trim().strip_prefix("$ "))
                .unwrap_or("")
                .to_string();
            Some(Request {
                tool: "Command".into(),
                detail,
                question,
            })
        }
        Harness::Cursor => {
            let q = ls.iter().rposition(|l| l.trim().starts_with("Run this"))?;
            let question = clean(ls[q]);
            let above = ls[..q]
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty() && !l.contains('─'))?;
            let above = above.trim();
            if question.contains("MCP") {
                return Some(Request {
                    tool: "MCP tool".into(),
                    detail: above.to_string(),
                    question,
                });
            }
            let cmd = above
                .trim_start_matches('$')
                .trim()
                .trim_end_matches(" in .")
                .to_string();
            Some(Request {
                tool: "Command".into(),
                detail: cmd,
                question,
            })
        }
        _ => {
            let r = ls
                .iter()
                .position(|l| l.contains("Requesting permission for"))?;
            let detail = ls[r + 1..]
                .iter()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_string();
            let question = ls[r + 1..]
                .iter()
                .map(|l| l.trim())
                .find(|l| l.ends_with('?'))
                .unwrap_or("Run this command?")
                .to_string();
            Some(Request {
                tool: "Command".into(),
                detail,
                question,
            })
        }
    }
}

pub fn exit_session_id(h: Harness, screen: &str) -> Option<String> {
    let word = |s: &str| {
        s.split_whitespace()
            .next()
            .map(|w| w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()))
            .filter(|w| Harness::safe_session_id(w) && w.len() >= 8)
            .filter(|w| w.chars().any(|c| c.is_ascii_digit()))
            .map(str::to_string)
    };
    let after = |l: &str, marker: &str| l.find(marker).and_then(|i| word(&l[i + marker.len()..]));

    screen.lines().rev().find_map(|l| match h {
        Harness::Codex => after(l, "codex resume "),
        Harness::Cursor => after(l, "--resume="),
        Harness::Antigravity => after(l, "--conversation="),
        Harness::OpenCode => after(l, "opencode -s ").or_else(|| after(l, "opencode --session ")),
        Harness::Claude | Harness::Grok => None,
    })
}

impl crate::app::App {
    pub(crate) fn learn_exit_session(&mut self, slot: usize, tab: usize) {
        let t = &mut self.panes[slot].tabs[tab];
        if t.harness.integrated() {
            return;
        }
        let screen = t
            .parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen()
            .contents();
        if let Some(id) = exit_session_id(t.harness, &screen) {
            if t.session_id.as_deref() != Some(&id) {
                t.session_id = Some(id);
                self.state_dirty = true;
            }
        }
    }
}

fn braille(c: char) -> bool {
    ('\u{2800}'..='\u{28ff}').contains(&c)
}

pub fn activity_for(h: Harness, text: &str) -> Option<Activity> {
    let ls = lines(text);
    let any = |f: &dyn Fn(&str) -> bool| ls.iter().any(|l| f(l.trim_start()));
    match h {
        Harness::Codex => Some(
            if parse_prompt_for(h, text).is_some() || text.contains("Press enter to continue") {
                Activity::Permission
            } else if text.contains("esc to interrupt") {
                Activity::Working
            } else if any(&|l| l.starts_with('›')) {
                Activity::Ready
            } else {
                Activity::Starting
            },
        ),
        Harness::Cursor => Some(
            if text.contains("Workspace Trust Required")
                || parse_prompt_for(h, text).is_some()
                || text.contains("Tell the agent what to do instead")
            {
                Activity::Permission
            } else if text.contains("ctrl+c to stop") {
                Activity::Working
            } else if any(&|l| l.starts_with("→ ")) {
                Activity::Ready
            } else {
                Activity::Starting
            },
        ),
        Harness::Antigravity => Some(
            if parse_prompt_for(h, text).is_some() || text.contains("Do you trust the contents") {
                Activity::Permission
            } else if text.contains("esc to cancel") {
                if any(&|l| l.chars().next().is_some_and(braille)) {
                    Activity::Working
                } else {
                    Activity::Permission
                }
            } else if text.contains("? for shortcuts") || any(&|l| l == ">" || l.starts_with("> "))
            {
                Activity::Ready
            } else {
                Activity::Starting
            },
        ),
        Harness::OpenCode => Some(if text.contains("esc interrupt") {
            Activity::Working
        } else if text.contains("ctrl+p commands")
            || text.contains("Ask anything")
            || any(&|l| l.starts_with("╹▀▀"))
        {
            Activity::Ready
        } else {
            Activity::Starting
        }),
        Harness::Claude | Harness::Grok => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::Choice;

    fn screen(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/screens/{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn keys(h: Harness, name: &str, c: Choice) -> Vec<u8> {
        let p = parse_prompt_for(h, &screen(name)).unwrap_or_else(|| panic!("{name}: no prompt"));
        let i = p
            .index_for(c)
            .unwrap_or_else(|| panic!("{name}: no option for {c:?}: {:?}", p.options));
        p.keys_for(i, false)
    }

    fn label(h: Harness, name: &str, c: Choice) -> String {
        let p = parse_prompt_for(h, &screen(name)).unwrap();
        p.options[p.index_for(c).unwrap()].label.clone()
    }

    #[test]
    fn real_screens_are_classified() {
        use Activity::*;
        let cases: &[(Harness, &str, Activity)] = &[
            (Harness::Codex, "codex_trust", Permission),
            (Harness::Codex, "codex_update", Permission),
            (Harness::Codex, "codex_ready", Ready),
            (Harness::Codex, "codex_working", Working),
            (Harness::Codex, "codex_done", Ready),
            (Harness::Codex, "codex_approval", Permission),
            (Harness::Codex, "codex_approval_wrapped", Permission),
            (Harness::Codex, "codex_mcp_approval", Permission),
            (Harness::Cursor, "cursor_trust", Permission),
            (Harness::Cursor, "cursor_ready", Ready),
            (Harness::Cursor, "cursor_working", Working),
            (Harness::Cursor, "cursor_done", Ready),
            (Harness::Cursor, "cursor_approval", Permission),
            (Harness::Cursor, "cursor_mcp_approval", Permission),
            (Harness::Cursor, "cursor_tellinstead", Permission),
            (Harness::Antigravity, "antigravity_trust", Permission),
            (Harness::Antigravity, "antigravity_trust_narrow", Permission),
            (Harness::Antigravity, "antigravity_ready", Ready),
            (Harness::Antigravity, "antigravity_working", Working),
            (Harness::Antigravity, "antigravity_done", Ready),
            (Harness::Antigravity, "antigravity_approval", Permission),
            (Harness::OpenCode, "opencode_ready", Ready),
            (Harness::OpenCode, "opencode_working", Working),
            (Harness::OpenCode, "opencode_draft_narrow", Ready),
        ];
        for &(h, name, want) in cases {
            assert_eq!(activity_for(h, &screen(name)), Some(want), "{name}");
        }

        assert_eq!(activity_for(Harness::Claude, "x"), None);
        assert_eq!(activity_for(Harness::Grok, "x"), None);

        for h in [
            Harness::Codex,
            Harness::Cursor,
            Harness::Antigravity,
            Harness::OpenCode,
        ] {
            assert_eq!(activity_for(h, ""), Some(Starting));
        }
    }

    #[test]
    fn codex_keys_are_the_ones_its_prompt_shows() {
        let h = Harness::Codex;

        assert_eq!(keys(h, "codex_approval", Choice::Approve), b"y");
        assert_eq!(keys(h, "codex_approval", Choice::Always), b"p");
        assert_eq!(keys(h, "codex_approval", Choice::Deny), b"\x1b");
        assert_eq!(label(h, "codex_approval", Choice::Approve), "Yes, proceed");
        let p = parse_prompt_for(h, &screen("codex_approval")).unwrap();
        assert_eq!(p.kind, PromptKind::Permission);
        assert_eq!(p.selected, 0);
        assert_eq!(p.options.len(), 3);

        assert_eq!(keys(h, "codex_approval_wrapped", Choice::Approve), b"y");
        assert_eq!(keys(h, "codex_approval_wrapped", Choice::Always), b"p");
        assert_eq!(keys(h, "codex_approval_wrapped", Choice::Deny), b"\x1b");
        assert_eq!(
            parse_prompt_for(h, &screen("codex_approval_wrapped"))
                .unwrap()
                .options
                .len(),
            3
        );

        assert_eq!(keys(h, "codex_mcp_approval", Choice::Approve), b"1");
        assert_eq!(keys(h, "codex_mcp_approval", Choice::Always), b"3");
        assert_eq!(keys(h, "codex_mcp_approval", Choice::Deny), b"4");
        assert_eq!(label(h, "codex_mcp_approval", Choice::Approve), "Allow");

        let p = parse_prompt_for(h, &screen("codex_trust")).unwrap();
        assert_eq!(p.kind, PromptKind::Trust);
        assert_eq!(keys(h, "codex_trust", Choice::Approve), b"1");
        assert_eq!(keys(h, "codex_trust", Choice::Deny), b"2");

        for n in ["codex_ready", "codex_working", "codex_done"] {
            assert!(parse_prompt_for(h, &screen(n)).is_none(), "{n}");
        }
    }

    #[test]
    fn cursor_keys_are_the_ones_its_prompt_shows() {
        let h = Harness::Cursor;
        assert_eq!(keys(h, "cursor_approval", Choice::Approve), b"y");
        assert_eq!(keys(h, "cursor_approval", Choice::Always), b"\t");
        assert_eq!(keys(h, "cursor_approval", Choice::Deny), b"n");
        assert_eq!(label(h, "cursor_approval", Choice::Approve), "Run (once)");

        for n in ["cursor_approval_cut", "cursor_approval_wrapped"] {
            assert_eq!(keys(h, n, Choice::Approve), b"y", "{n}");
            assert_eq!(keys(h, n, Choice::Always), b"\t", "{n}");
            assert_eq!(keys(h, n, Choice::Deny), b"n", "{n}");
            let p = parse_prompt_for(h, &screen(n)).unwrap();
            assert_eq!(p.options.len(), 4, "{n}: {:?}", p.options);
        }

        let p = parse_prompt_for(h, &screen("cursor_approval")).unwrap();
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert!(labels.iter().any(|l| l.starts_with("Run Everything")));
        for c in [Choice::Approve, Choice::Always, Choice::Deny] {
            assert!(!label(h, "cursor_approval", c).starts_with("Run Everything"));
        }
        assert_eq!(keys(h, "cursor_mcp_approval", Choice::Approve), b"y");
        assert_eq!(keys(h, "cursor_mcp_approval", Choice::Always), b"\t");
        assert_eq!(keys(h, "cursor_mcp_approval", Choice::Deny), b"n");
        assert_eq!(keys(h, "cursor_trust", Choice::Approve), b"a");
        assert_eq!(keys(h, "cursor_trust", Choice::Deny), b"q");
        assert_eq!(
            parse_prompt_for(h, &screen("cursor_trust")).unwrap().kind,
            PromptKind::Trust
        );

        for n in [
            "cursor_tellinstead",
            "cursor_ready",
            "cursor_working",
            "cursor_done",
        ] {
            assert!(parse_prompt_for(h, &screen(n)).is_none(), "{n}");
        }
    }

    #[test]
    fn antigravity_keys_are_the_ones_its_prompt_shows() {
        let h = Harness::Antigravity;

        assert_eq!(keys(h, "antigravity_approval", Choice::Approve), b"1");
        assert_eq!(keys(h, "antigravity_approval", Choice::Always), b"2");
        assert_eq!(keys(h, "antigravity_approval", Choice::Deny), b"4");

        assert_eq!(
            keys(h, "antigravity_approval_wrapped", Choice::Approve),
            b"1"
        );
        assert_eq!(
            keys(h, "antigravity_approval_wrapped", Choice::Always),
            b"2"
        );
        assert_eq!(keys(h, "antigravity_approval_wrapped", Choice::Deny), b"4");
        assert!(label(h, "antigravity_approval_wrapped", Choice::Always)
            .contains("in this conversation"));

        assert!(label(h, "antigravity_approval", Choice::Always).contains("in this conversation"));

        let p = parse_prompt_for(h, &screen("antigravity_trust")).unwrap();
        assert_eq!(p.kind, PromptKind::Trust);
        assert_eq!(p.selected, 0);
        assert_eq!(keys(h, "antigravity_trust", Choice::Approve), b"\r");
        assert_eq!(keys(h, "antigravity_trust", Choice::Deny), b"\x1b[B\r");

        let p = parse_prompt_for(h, &screen("antigravity_trust_narrow")).unwrap();
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Yes, I trust this folder", "No, exit"]);
        assert_eq!(keys(h, "antigravity_trust_narrow", Choice::Approve), b"\r");
        assert_eq!(
            keys(h, "antigravity_trust_narrow", Choice::Deny),
            b"\x1b[B\r"
        );

        let moved = screen("antigravity_trust_narrow")
            .replace("> Yes, I trust", "  Yes, I trust")
            .replace("  No, exit", "> No, exit");
        let p = parse_prompt_for(h, &moved).unwrap();
        assert_eq!(p.selected, 1);
        assert_eq!(p.keys_for(p.index_for(Choice::Deny).unwrap(), false), b"\r");
        assert_eq!(
            p.keys_for(p.index_for(Choice::Approve).unwrap(), false),
            b"\x1b[A\r"
        );
        for n in [
            "antigravity_ready",
            "antigravity_working",
            "antigravity_done",
        ] {
            assert!(parse_prompt_for(h, &screen(n)).is_none(), "{n}");
        }
    }

    #[test]
    fn requests_name_the_command_or_tool() {
        let r = |h, n: &str| describe_request_for(h, &screen(n)).unwrap();
        let x = r(Harness::Codex, "codex_approval");
        assert_eq!(x.tool, "Command");
        assert_eq!(x.detail, "printf 'hello' > t.txt");
        assert!(x.question.contains("Would you like to run"));
        let x = r(Harness::Codex, "codex_mcp_approval");
        assert_eq!(
            (x.tool.as_str(), x.detail.as_str()),
            ("MCP tool", "godterm list_jobs")
        );
        let x = r(Harness::Codex, "codex_trust");
        assert_eq!(x.tool, "Trust folder");
        assert_eq!(x.detail, "/tmp/work");
        let x = r(Harness::Cursor, "cursor_approval");
        assert_eq!(x.detail, "curl -sI https://example.com");
        let x = r(Harness::Cursor, "cursor_mcp_approval");
        assert_eq!(x.tool, "MCP tool");
        assert!(x.detail.contains("list_jobs"), "{x:?}");
        let x = r(Harness::Cursor, "cursor_trust");
        assert_eq!(x.detail, "/tmp/work");
        let x = r(Harness::Antigravity, "antigravity_approval");
        assert_eq!((x.tool.as_str(), x.detail.as_str()), ("Command", "ls -la"));
        let x = r(Harness::Antigravity, "antigravity_trust");
        assert_eq!(x.detail, "/tmp/work");
        assert!(describe_request_for(Harness::Codex, &screen("codex_ready")).is_none());
    }

    #[test]
    fn a_quit_cli_names_the_session_to_resume() {
        let id = |h, n: &str| exit_session_id(h, &screen(n));
        assert_eq!(
            id(Harness::Codex, "codex_exit").as_deref(),
            Some("01a160a4-3e5b-70c0-b5f2-602bc3896cd4")
        );
        assert_eq!(
            id(Harness::Cursor, "cursor_exit").as_deref(),
            Some("297f9efd-116c-4db4-befd-98e60b1d0fb7")
        );
        assert_eq!(
            id(Harness::Antigravity, "antigravity_exit").as_deref(),
            Some("03a2b777-bc64-42d5-b8df-070dd1bd07f4")
        );
        assert_eq!(
            id(Harness::OpenCode, "opencode_exit").as_deref(),
            Some("ses_e9f4fdae3ffe39HU3KgE7Wdrdj")
        );

        assert_eq!(id(Harness::Cursor, "codex_exit"), None);
        assert_eq!(id(Harness::Codex, "codex_working"), None);
        assert_eq!(id(Harness::Claude, "codex_exit"), None);
        assert_eq!(
            exit_session_id(Harness::Codex, "codex resume --sandbox"),
            None
        );
        assert_eq!(exit_session_id(Harness::Codex, "codex resume ;rm"), None);
    }

    #[test]
    fn partial_and_garbled_screens_never_panic() {
        let names = [
            "codex_approval",
            "codex_mcp_approval",
            "cursor_approval",
            "cursor_trust",
            "antigravity_approval",
            "antigravity_trust",
            "opencode_working",
        ];
        for n in names {
            let s = screen(n);
            let cuts: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
            for &i in cuts.iter().step_by(7) {
                for h in [
                    Harness::Codex,
                    Harness::Cursor,
                    Harness::Antigravity,
                    Harness::OpenCode,
                ] {
                    let part = &s[..i];
                    let _ = activity_for(h, part);
                    if let Some(p) = parse_prompt_for(h, part) {
                        for c in [Choice::Approve, Choice::Always, Choice::Deny] {
                            if let Some(k) = p.index_for(c) {
                                let _ = p.keys_for(k, true);
                            }
                        }
                    }
                    let _ = describe_request_for(h, part);
                }
            }
        }
    }
}
