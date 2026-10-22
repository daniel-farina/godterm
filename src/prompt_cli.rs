//! What Codex, Cursor, Antigravity and OpenCode screens look like: their
//! approval and trust prompts (with the keys each really takes) and
//! whether a tab is working or waiting for input. Read off the real CLIs
//! (codex 0.153.4, cursor-agent 2026.10.19, agy 1.4.4, opencode 2.0.32);
//! the captures are tests/fixtures/screens/{codex,cursor,antigravity,
//! opencode}_*.txt. OpenCode's permission prompt was not captured (it had
//! no model to ask), so its prompts are left to the keyboard.

use crate::harness::Harness;
use crate::pane::Activity;
use crate::prompt::{Prompt, PromptKind, PromptOption, Request};

fn lines(screen: &str) -> Vec<&str> {
    screen.lines().map(str::trim_end).collect()
}

/// "label  description" (two or more spaces between) -> "label".
fn column_label(s: &str) -> &str {
    match s.find("  ") {
        Some(i) => s[..i].trim_end(),
        None => s.trim_end(),
    }
}

/// A trailing "(y)", "(esc)", "(esc or n)", "(shift+tab)": the label
/// without it and the key it names.
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

/// "  › 2. label" / "> 1. label" / "    3. label": (selected, number, label).
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

/// The last run of numbered option lines: (index of its first line,
/// (selected, number, label) per option).
fn last_numbered_block(ls: &[&str]) -> Option<(usize, Vec<(bool, u32, String)>)> {
    let mut end = None;
    for i in (0..ls.len()).rev() {
        if numbered(ls[i]).is_some() {
            end = Some(i);
            break;
        }
    }
    let end = end?;
    let mut start = end;
    while start > 0 && numbered(ls[start - 1]).is_some() {
        start -= 1;
    }
    let opts: Vec<_> = (start..=end).filter_map(|i| numbered(ls[i])).collect();
    (opts.len() >= 2).then_some((start, opts))
}

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

/// Which options approve, always-approve and deny, by wording.
fn pick_by_label(labels: &[String]) -> [Option<usize>; 3] {
    let low: Vec<String> = labels.iter().map(|l| l.to_lowercase()).collect();
    let find = |f: &dyn Fn(&str) -> bool| low.iter().position(|l| f(l));
    let always = |l: &str| {
        l.contains("don't ask again") || l.starts_with("always allow") || l.contains("always")
    };
    [
        find(&|l| (l.starts_with("yes") && !always(l)) || l == "allow" || l.starts_with("allow once")),
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
        // "▶ [a] Trust this workspace" / "  [q] Quit": the letter is the key.
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
            return Some(finish(PromptKind::Trust, opts, selected, [Some(0), None, Some(1)]));
        }
        return None;
    }
    // "Run this command?" / "Run this MCP tool?" then one option per line,
    // each ending in its key: "→ Run (once) (y)".
    let q = ls.iter().rposition(|l| {
        let t = l.trim();
        t.starts_with("Run this") && t.ends_with('?')
    })?;
    let mut opts = vec![];
    let mut selected = 0;
    for l in &ls[q + 1..] {
        let t = l.trim_end();
        let body = t.trim_start();
        let (sel, body) = match body.strip_prefix('→') {
            Some(r) => (true, r.trim_start()),
            None => (false, body),
        };
        let (label, key) = split_hotkey(body);
        if key.is_some() {
            if sel {
                selected = opts.len();
            }
            opts.push((label, None, key));
        }
    }
    if opts.len() < 2 {
        return None;
    }
    let low: Vec<String> = opts.iter().map(|o| o.0.to_lowercase()).collect();
    let find = |f: &dyn Fn(&str) -> bool| low.iter().position(|l| f(l));
    let pick = [
        find(&|l| l.starts_with("run (once)")),
        find(&|l| l.starts_with("add ") || l.starts_with("allowlist")),
        // "Skip (esc or n)" declines; "Reject & propose changes" asks the
        // agent for another way and is not what a plain no means.
        find(&|l| l.starts_with("skip")).or_else(|| find(&|l| l.starts_with("reject"))),
    ];
    Some(finish(PromptKind::Permission, opts, selected, pick))
}

fn antigravity_prompt(screen: &str) -> Option<Prompt> {
    let ls = lines(screen);
    if screen.contains("Do you trust the contents of this project") {
        // "> Yes, I trust this folder" / "  No, exit": arrows and Enter.
        let q = ls.iter().position(|l| l.contains("Do you trust the contents"))?;
        let mut opts = vec![];
        let mut selected = 0;
        for l in &ls[q + 1..] {
            let t = l.trim_end();
            if t.trim().is_empty() || t.contains("Navigate") {
                if opts.is_empty() {
                    continue;
                }
                break;
            }
            if t.contains("Antigravity CLI requires") {
                continue;
            }
            let (sel, label) = match t.strip_prefix("> ") {
                Some(r) => (true, r),
                None => (false, t.trim_start()),
            };
            if sel {
                selected = opts.len();
            }
            opts.push((label.trim().to_string(), None, None));
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

/// The prompt a tab of harness `h` shows, with the keys that pick each
/// option. Claude, Grok and OpenCode use the generic reader.
pub fn parse_prompt_for(h: Harness, screen: &str) -> Option<Prompt> {
    match h {
        Harness::Codex => codex_prompt(screen),
        Harness::Cursor => cursor_prompt(screen),
        Harness::Antigravity => antigravity_prompt(screen),
        _ => crate::prompt::parse_prompt(screen),
    }
}

/// What the prompt is about, for the approvals queue and the voice.
pub fn describe_request_for(h: Harness, screen: &str) -> Option<Request> {
    if !matches!(h, Harness::Codex | Harness::Cursor | Harness::Antigravity) {
        return crate::prompt::describe_request(screen);
    }
    let p = parse_prompt_for(h, screen)?;
    let ls = lines(screen);
    let clean = |s: &str| s.trim().to_string();
    if p.kind == PromptKind::Trust {
        // Codex: "> You are in <dir>"; Antigravity: the line under
        // "Accessing workspace:"; Cursor: the line under the question.
        let after = |marker: &str| {
            ls.iter()
                .position(|l| l.contains(marker))
                .and_then(|i| {
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
                l.contains("Would you like to") || l.contains("Do you want to") || l.contains("Allow the")
            })?;
            let question = clean(ls[q]);
            if question.contains("MCP server") {
                let tool = question
                    .split('"')
                    .nth(1)
                    .unwrap_or("")
                    .to_string();
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
            let above = ls[..q].iter().rev().find(|l| !l.trim().is_empty() && !l.contains('─'))?;
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
            let r = ls.iter().position(|l| l.contains("Requesting permission for"))?;
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

/// The session id a CLI prints when the user quits it ("codex resume
/// <id>", "agent --resume=<id>", "agy --conversation=<id>", "opencode -s
/// <id>"): GodTerm discovers no other session ids for these CLIs, so this
/// is how a tab that was quit (not killed) can be resumed later.
pub fn exit_session_id(h: Harness, screen: &str) -> Option<String> {
    let word = |s: &str| {
        s.split_whitespace()
            .next()
            .map(|w| w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()))
            // "codex resume and select ..." names no session: an id is
            // long and has a digit (UUIDs, "ses_...").
            .filter(|w| Harness::safe_session_id(w) && w.len() >= 8)
            .filter(|w| w.chars().any(|c| c.is_ascii_digit()))
            .map(str::to_string)
    };
    let after = |l: &str, marker: &str| l.find(marker).and_then(|i| word(&l[i + marker.len()..]));
    // The last one printed: a long session may show several screens.
    screen.lines().rev().find_map(|l| match h {
        Harness::Codex => after(l, "codex resume "),
        Harness::Cursor => after(l, "--resume="),
        Harness::Antigravity => after(l, "--conversation="),
        Harness::OpenCode => after(l, "opencode -s ").or_else(|| after(l, "opencode --session ")),
        Harness::Claude | Harness::Grok => None,
    })
}

impl crate::app::App {
    /// A tab of one of these CLIs just exited on its own: keep the session
    /// id its farewell screen names, so Enter, a restart of GodTerm or
    /// "reopen closed tab" resume that conversation instead of a new one.
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

/// What a tab of harness `h` is doing, read off its screen; None for the
/// CLIs `pane::detect_activity` already knows (Claude, Grok).
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
                // A spinner line is a turn; the same footer under a menu
                // (a dialog with nothing running) is the user's to answer.
                if any(&|l| l.chars().next().is_some_and(braille)) {
                    Activity::Working
                } else {
                    Activity::Permission
                }
            } else if text.contains("? for shortcuts") || any(&|l| l == ">" || l.starts_with("> ")) {
                Activity::Ready
            } else {
                Activity::Starting
            },
        ),
        Harness::OpenCode => Some(if text.contains("esc interrupt") {
            Activity::Working
        } else if text.contains("ctrl+p commands") || text.contains("Ask anything") {
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
            (Harness::Codex, "codex_mcp_approval", Permission),
            (Harness::Cursor, "cursor_trust", Permission),
            (Harness::Cursor, "cursor_ready", Ready),
            (Harness::Cursor, "cursor_working", Working),
            (Harness::Cursor, "cursor_done", Ready),
            (Harness::Cursor, "cursor_approval", Permission),
            (Harness::Cursor, "cursor_mcp_approval", Permission),
            (Harness::Cursor, "cursor_tellinstead", Permission),
            (Harness::Antigravity, "antigravity_trust", Permission),
            (Harness::Antigravity, "antigravity_ready", Ready),
            (Harness::Antigravity, "antigravity_working", Working),
            (Harness::Antigravity, "antigravity_done", Ready),
            (Harness::Antigravity, "antigravity_approval", Permission),
            (Harness::OpenCode, "opencode_ready", Ready),
            (Harness::OpenCode, "opencode_working", Working),
        ];
        for &(h, name, want) in cases {
            assert_eq!(activity_for(h, &screen(name)), Some(want), "{name}");
        }
        // Claude and Grok keep the generic detector.
        assert_eq!(activity_for(Harness::Claude, "x"), None);
        assert_eq!(activity_for(Harness::Grok, "x"), None);
        // An empty or half drawn screen is just starting.
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
        // "1. Yes, proceed (y)", "2. ... don't ask again ... (p)", "3. No ... (esc)"
        assert_eq!(keys(h, "codex_approval", Choice::Approve), b"y");
        assert_eq!(keys(h, "codex_approval", Choice::Always), b"p");
        assert_eq!(keys(h, "codex_approval", Choice::Deny), b"\x1b");
        assert_eq!(label(h, "codex_approval", Choice::Approve), "Yes, proceed");
        let p = parse_prompt_for(h, &screen("codex_approval")).unwrap();
        assert_eq!(p.kind, PromptKind::Permission);
        assert_eq!(p.selected, 0);
        assert_eq!(p.options.len(), 3);
        // The MCP prompt has no shortcuts: the numbers pick (verified).
        assert_eq!(keys(h, "codex_mcp_approval", Choice::Approve), b"1");
        assert_eq!(keys(h, "codex_mcp_approval", Choice::Always), b"3");
        assert_eq!(keys(h, "codex_mcp_approval", Choice::Deny), b"4");
        assert_eq!(label(h, "codex_mcp_approval", Choice::Approve), "Allow");
        // Folder trust.
        let p = parse_prompt_for(h, &screen("codex_trust")).unwrap();
        assert_eq!(p.kind, PromptKind::Trust);
        assert_eq!(keys(h, "codex_trust", Choice::Approve), b"1");
        assert_eq!(keys(h, "codex_trust", Choice::Deny), b"2");
        // Not prompts: the composer, a running turn, the finished turn.
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
        // "Run Everything (shift+tab)" is never what a spoken yes means.
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
        // The "tell the agent" box and the plain screens are not menus.
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
        // Numbers pick at once (verified: 1 ran the command, 4 declined).
        assert_eq!(keys(h, "antigravity_approval", Choice::Approve), b"1");
        assert_eq!(keys(h, "antigravity_approval", Choice::Always), b"2");
        assert_eq!(keys(h, "antigravity_approval", Choice::Deny), b"4");
        // "Always" is the conversation-wide one, not the persisted rule.
        assert!(label(h, "antigravity_approval", Choice::Always).contains("in this conversation"));
        // Folder trust has no numbers: arrows and Enter from the highlight.
        let p = parse_prompt_for(h, &screen("antigravity_trust")).unwrap();
        assert_eq!(p.kind, PromptKind::Trust);
        assert_eq!(p.selected, 0);
        assert_eq!(keys(h, "antigravity_trust", Choice::Approve), b"\r");
        assert_eq!(keys(h, "antigravity_trust", Choice::Deny), b"\x1b[B\r");
        for n in ["antigravity_ready", "antigravity_working", "antigravity_done"] {
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
        // Another CLI's farewell, a running screen and a hostile one: nothing.
        assert_eq!(id(Harness::Cursor, "codex_exit"), None);
        assert_eq!(id(Harness::Codex, "codex_working"), None);
        assert_eq!(id(Harness::Claude, "codex_exit"), None);
        assert_eq!(exit_session_id(Harness::Codex, "codex resume --sandbox"), None);
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
