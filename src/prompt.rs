//! Read the selection prompt claude is showing (permission, folder trust,
//! plan approval, login method...) so voice and keys can pick the right
//! option instead of assuming fixed numbers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// "Do you want to proceed?" / "Do you want to make this edit...".
    Permission,
    /// "Accessing workspace: ... Yes, I trust this folder".
    Trust,
    /// "Claude has written up a plan and is ready to execute".
    Plan,
    /// The one time "WARNING: Claude Code running in Bypass Permissions
    /// mode" dialog (Yes, I accept / No, exit).
    Bypass,
    /// Any other picker (theme, login method, ...).
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptOption {
    pub label: String,
    pub number: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub options: Vec<PromptOption>,
    /// Index of the highlighted option.
    pub selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Approve,
    Always,
    Deny,
}

/// Strip box drawing borders a screen line may have on either side.
fn unbox(line: &str) -> &str {
    let t = line.trim_end();
    let t = t.strip_prefix('│').unwrap_or(t);
    let t = t.trim_end();
    t.strip_suffix('│').unwrap_or(t)
}

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| matches!(c, '─' | '━' | '╌' | '-' | '═'))
}

/// Split "  3. No, and tell..." into (indent cols, number, label).
fn option_parts(s: &str) -> Option<(usize, Option<u32>, String)> {
    let indent = s.chars().take_while(|c| *c == ' ').count();
    let rest: String = s.chars().skip(indent).collect();
    if rest.is_empty() {
        return None;
    }
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let after = &rest[digits.len()..];
        if let Some(label) = after.strip_prefix(". ") {
            return Some((indent, digits.parse().ok(), label.trim().to_string()));
        }
    }
    Some((indent, None, rest.trim().to_string()))
}

pub fn parse_prompt(screen: &str) -> Option<Prompt> {
    let lines: Vec<&str> = screen.lines().map(unbox).collect();
    // The highlighted option: a line with the pointer that is not the
    // prompt input box (which sits right under a horizontal rule).
    let sel = (0..lines.len()).rev().find(|&i| {
        let t = lines[i].trim_start();
        let Some(rest) = t
            .strip_prefix('❯')
            .or_else(|| t.strip_prefix('›'))
            .or_else(|| t.strip_prefix('▶'))
        else {
            return false;
        };
        let under_rule = i > 0 && is_rule(lines[i - 1]);
        !rest.trim().is_empty() && !under_rule
    })?;
    let sel_line = lines[sel];
    let pointer_col = sel_line.chars().take_while(|c| *c == ' ').count();
    // Text column: after the pointer and its space.
    let text_col = pointer_col + 2;
    let parse_at = |l: &str, selected: bool| -> Option<PromptOption> {
        let chars: Vec<char> = l.chars().collect();
        if chars.len() <= text_col {
            return None;
        }
        let prefix: String = chars[..text_col].iter().collect();
        let body: String = chars[text_col..].iter().collect();
        let prefix_ok = if selected {
            prefix.trim() == "❯" || prefix.trim() == "›" || prefix.trim() == "▶"
        } else {
            prefix.trim().is_empty()
        };
        if !prefix_ok || body.starts_with(' ') || body.trim().is_empty() {
            return None;
        }
        let (_, number, label) = option_parts(&body)?;
        Some(PromptOption { label, number })
    };
    let first = parse_at(sel_line, true)?;
    let numbered = first.number.is_some();
    let mut above = vec![];
    let mut i = sel;
    while i > 0 {
        i -= 1;
        match parse_at(lines[i], false) {
            Some(o) if o.number.is_some() == numbered && (numbered || above.len() < 6) => {
                above.push(o)
            }
            _ => {
                // Wrapped continuation lines are indented deeper; skip them.
                let deeper = lines[i].chars().take_while(|c| *c == ' ').count() > text_col;
                if deeper && numbered {
                    continue;
                }
                break;
            }
        }
    }
    above.reverse();
    // For unnumbered prompts, text above the options can look like options;
    // keep only lines that read like choices.
    if !numbered {
        above.retain(|o| looks_like_choice(&o.label));
    }
    let selected = above.len();
    let mut options = above;
    options.push(first);
    let mut j = sel + 1;
    while j < lines.len() {
        match parse_at(lines[j], false) {
            Some(o) if o.number.is_some() == numbered => {
                if !numbered && !looks_like_choice(&o.label) {
                    break;
                }
                options.push(o)
            }
            _ => {
                let deeper = lines[j].chars().take_while(|c| *c == ' ').count() > text_col;
                if deeper && numbered && !lines[j].trim().is_empty() {
                    j += 1;
                    continue;
                }
                break;
            }
        }
        j += 1;
    }
    if options.len() < 2 {
        return None;
    }
    let kind = if screen.contains("Bypass Permissions mode")
        && options.iter().any(|o| o.label.starts_with("Yes, I accept"))
    {
        PromptKind::Bypass
    } else if screen.contains("trust this folder") || screen.contains("Accessing workspace") {
        PromptKind::Trust
    } else if screen.contains("ready to execute") || screen.contains("keep planning") {
        PromptKind::Plan
    } else if screen.contains("Do you want to")
        || options.iter().any(|o| {
            o.label.starts_with("No, and tell Claude")
                || o.label == "Reject"
                || o.label.starts_with("Allow once")
        })
    {
        PromptKind::Permission
    } else {
        PromptKind::Other
    };
    Some(Prompt {
        kind,
        options,
        selected,
    })
}

fn looks_like_choice(label: &str) -> bool {
    let l = label.to_lowercase();
    l.starts_with("yes")
        || l.starts_with("no")
        || l.starts_with("allow")
        || l.starts_with("deny")
        || l.starts_with("cancel")
        || l.starts_with("continue")
        || l.starts_with("exit")
        || l.starts_with("reject")
        || l.starts_with("always allow")
}

impl Prompt {
    /// Which option a spoken or typed answer means.
    pub fn index_for(&self, c: Choice) -> Option<usize> {
        let low: Vec<String> = self
            .options
            .iter()
            .map(|o| o.label.to_lowercase())
            .collect();
        let find = |pred: &dyn Fn(&str) -> bool| low.iter().position(|l| pred(l));
        match c {
            Choice::Approve => find(&|l| l.starts_with("allow once")).or_else(|| {
                find(&|l| {
                    l.starts_with("yes")
                        && !l.contains("don't ask")
                        && !l.contains("allow all")
                        && !l.contains("always")
                        && !l.contains("auto-accept")
                })
                .or_else(|| find(&|l| l.starts_with("yes")))
            }),
            // grok: "Always allow this command" (this one), before "on all sessions".
            Choice::Always => find(&|l| l.starts_with("always allow this"))
                .or_else(|| find(&|l| l.starts_with("always allow")))
                .or_else(|| {
                    find(&|l| {
                        l.starts_with("yes")
                            && (l.contains("don't ask")
                                || l.contains("always")
                                || l.contains("allow all")
                                || l.contains("auto-accept"))
                    })
                }),
            Choice::Deny => find(&|l| l.starts_with("no") || l.starts_with("reject")),
        }
    }

    /// Key bytes that select option `idx`: its number when shown, otherwise
    /// arrow presses from the highlighted option followed by Enter.
    pub fn keys_for(&self, idx: usize, app_cursor: bool) -> Vec<u8> {
        if let Some(n) = self.options.get(idx).and_then(|o| o.number) {
            if n < 10 {
                return n.to_string().into_bytes();
            }
        }
        let (up, down): (&[u8], &[u8]) = if app_cursor {
            (b"\x1bOA", b"\x1bOB")
        } else {
            (b"\x1b[A", b"\x1b[B")
        };
        let mut out = vec![];
        if idx > self.selected {
            for _ in 0..idx - self.selected {
                out.extend_from_slice(down);
            }
        } else {
            for _ in 0..self.selected - idx {
                out.extend_from_slice(up);
            }
        }
        out.push(b'\r');
        out
    }
}

/// What a waiting prompt is asking for, for the approvals queue.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// "Bash command", "Edit file", "Trust folder", "Plan"...
    pub tool: String,
    /// The command, file or folder, when shown.
    pub detail: String,
    /// The question line ("Do you want to proceed?").
    pub question: String,
}

impl Request {
    pub fn summary(&self) -> String {
        if self.detail.is_empty() {
            self.tool.clone()
        } else {
            format!("{}: {}", self.tool, self.detail)
        }
    }
}

/// Read the tool and command or file a prompt is about from the lines above
/// its question.
pub fn describe_request(screen: &str) -> Option<Request> {
    let p = parse_prompt(screen)?;
    let lines: Vec<&str> = screen.lines().map(unbox).collect();
    let clean = |l: &str| l.trim().to_string();
    match p.kind {
        PromptKind::Trust => {
            let i = lines.iter().position(|l| l.contains("Accessing workspace"));
            // The path may wrap; join following lines that have no spaces.
            let mut detail = i
                .and_then(|i| lines.get(i + 1))
                .map(|l| clean(l))
                .unwrap_or_default();
            if let Some(i) = i {
                for l in lines.iter().skip(i + 2) {
                    let t = clean(l);
                    if t.is_empty() || t.contains(' ') {
                        break;
                    }
                    detail.push_str(&t);
                }
            }
            return Some(Request {
                tool: "Trust folder".into(),
                detail,
                question: "trust this folder?".into(),
            });
        }
        PromptKind::Plan => {
            let start = lines
                .iter()
                .position(|l| l.contains("Here is Claude's plan"));
            let detail = start
                .and_then(|i| {
                    lines[i + 1..]
                        .iter()
                        .map(|l| clean(l))
                        .find(|l| !l.is_empty() && !is_rule(l))
                })
                .unwrap_or_default();
            return Some(Request {
                tool: "Plan".into(),
                detail,
                question: "start on the plan?".into(),
            });
        }
        PromptKind::Bypass => {
            return Some(Request {
                tool: "Bypass permissions warning".into(),
                detail: "accept once for this account to run with --dangerously-skip-permissions"
                    .into(),
                question: "accept bypass permissions mode?".into(),
            })
        }
        PromptKind::Other => return None,
        PromptKind::Permission => {}
    }
    let q = lines.iter().rposition(|l| l.contains("Do you want to"))?;
    let question = clean(lines[q]);
    // Walk up to the top of the dialog: a box top, a rule, or 15 lines.
    let mut block: Vec<String> = vec![];
    let mut i = q;
    while i > 0 && q - i < 15 {
        i -= 1;
        let raw = lines[i];
        if raw.trim_start().starts_with('╭') || is_rule(raw) {
            break;
        }
        let t = clean(raw);
        if !t.is_empty() {
            block.push(t);
        }
    }
    block.reverse();
    // Edits show a diff between rules; the tool title sits above it.
    if block.is_empty()
        || block
            .iter()
            .all(|l| l.chars().next().is_some_and(|c| c.is_ascii_digit()))
    {
        // Skip the diff up to its opening rule, then read the title lines.
        let open = lines[..i].iter().rposition(|l| is_rule(l)).unwrap_or(i);
        let top = lines[..open]
            .iter()
            .rev()
            .take(15)
            .map(|l| clean(l))
            .filter(|l| !l.is_empty() && !is_rule(l));
        let mut above: Vec<String> = top.take(2).collect();
        above.reverse();
        block = above;
    }
    let tool = block
        .first()
        .cloned()
        .unwrap_or_else(|| "Permission".into());
    let mut detail = block.get(1).cloned().unwrap_or_default();
    if detail.is_empty() {
        // "Do you want to make this edit to main.rs?" names the file.
        if let Some(rest) = question
            .split(" to ")
            .last()
            .filter(|_| question.contains("edit"))
        {
            detail = rest.trim_end_matches('?').to_string();
        }
    }
    Some(Request {
        tool,
        detail,
        question,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/screens/{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn real_trust_prompt() {
        // Captured from claude 2.1.291 with a throwaway config dir.
        let p = parse_prompt(&screen("trust_prompt")).unwrap();
        assert_eq!(p.kind, PromptKind::Trust);
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, vec!["No, exit", "Yes, I trust this folder"]);
        assert_eq!(p.selected, 0);
        let yes = p.index_for(Choice::Approve).unwrap();
        assert_eq!(yes, 1);
        // Unnumbered: one Down then Enter.
        assert_eq!(p.keys_for(yes, false), b"\x1b[B\r");
        let moved = parse_prompt(&screen("trust_prompt_moved")).unwrap();
        assert_eq!(moved.selected, 1);
        assert_eq!(
            moved.keys_for(moved.index_for(Choice::Deny).unwrap(), false),
            b"\x1b[A\r"
        );
    }

    #[test]
    fn real_bypass_warning() {
        // Captured from claude 2.1.291 started with --dangerously-skip-permissions.
        let p = parse_prompt(&screen("bypass_warning")).unwrap();
        assert_eq!(p.kind, PromptKind::Bypass);
        assert_eq!(p.options[p.selected].label, "No, exit");
        let yes = p.index_for(Choice::Approve).unwrap();
        assert_eq!(p.options[yes].label, "Yes, I accept");
        assert_eq!(p.keys_for(yes, false), b"\x1b[B\r");
        assert_eq!(
            describe_request(&screen("bypass_warning")).unwrap().tool,
            "Bypass permissions warning"
        );
        assert_eq!(
            crate::pane::detect_activity(&screen("bypass_warning")),
            crate::pane::Activity::Permission
        );
    }

    #[test]
    fn real_login_and_input_screens() {
        let p = parse_prompt(&screen("login_method")).unwrap();
        assert_eq!(p.kind, PromptKind::Other);
        assert_eq!(p.options.len(), 3);
        assert_eq!(p.options[2].number, Some(3));
        // The ready input box ("❯ " under a rule) is not a prompt.
        assert!(parse_prompt(&screen("input_ready")).is_none());
        assert!(parse_prompt(&screen("security_notes")).is_none());
        // The theme picker is not a yes/no prompt.
        assert!(parse_prompt(&screen("theme")).is_none_or(|p| p.kind == PromptKind::Other));
    }

    #[test]
    fn permission_prompts() {
        let p = parse_prompt(&screen("permission_bash")).unwrap();
        assert_eq!(p.kind, PromptKind::Permission);
        assert_eq!(p.options.len(), 3);
        assert_eq!(
            p.keys_for(p.index_for(Choice::Approve).unwrap(), false),
            b"1"
        );
        assert_eq!(
            p.keys_for(p.index_for(Choice::Always).unwrap(), false),
            b"2"
        );
        assert_eq!(p.keys_for(p.index_for(Choice::Deny).unwrap(), false), b"3");

        let e = parse_prompt(&screen("permission_edit")).unwrap();
        assert_eq!(e.selected, 1);
        assert_eq!(
            e.options[1].label,
            "Yes, allow all edits during this session (shift+tab)"
        );
        assert_eq!(e.index_for(Choice::Approve), Some(0));
        assert_eq!(e.index_for(Choice::Always), Some(1));
    }

    #[test]
    fn plan_prompt() {
        let p = parse_prompt(&screen("plan_approval")).unwrap();
        assert_eq!(p.kind, PromptKind::Plan);
        assert_eq!(p.options.len(), 3);
        // The plan's own numbered steps above are not options.
        assert_eq!(p.options[0].label, "Yes, and auto-accept edits");
        assert_eq!(p.index_for(Choice::Approve), Some(1));
        assert_eq!(p.index_for(Choice::Always), Some(0));
        assert_eq!(p.index_for(Choice::Deny), Some(2));
    }

    #[test]
    fn describes_requests() {
        let r = describe_request(&screen("permission_bash")).unwrap();
        assert_eq!(r.tool, "Bash command");
        assert_eq!(r.detail, "rm -rf build");
        assert_eq!(r.summary(), "Bash command: rm -rf build");
        let e = describe_request(&screen("permission_edit")).unwrap();
        assert_eq!(
            (e.tool.as_str(), e.detail.as_str()),
            ("Edit file", "src/main.rs")
        );
        let t = describe_request(&screen("trust_prompt")).unwrap();
        assert_eq!(t.tool, "Trust folder");
        assert!(t.detail.ends_with("capdir/work"), "{}", t.detail);
        let p = describe_request(&screen("plan_approval")).unwrap();
        assert_eq!(
            (p.tool.as_str(), p.detail.as_str()),
            ("Plan", "1. Add the parser")
        );
        assert!(describe_request(&screen("login_method")).is_none());
        assert!(describe_request(&screen("input_ready")).is_none());
    }

    /// Fuzz style: random screens with pointers, digits, box drawing, wide
    /// and combining characters never panic the prompt reader.
    #[test]
    fn fuzz_prompt_parser() {
        let mut seed: u64 = 0xdead_beef_cafe_f00d;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let bits = [
            "❯",
            "›",
            " ",
            "  ",
            "1.",
            "2. ",
            "10. ",
            "Yes",
            "No, exit",
            "│",
            "╭",
            "─",
            "\n",
            "Do you want to",
            "Accessing workspace",
            "ready to execute",
            "日本語",
            "é",
            "\u{301}",
            "👍",
            "\t",
            "Here is Claude's plan",
            "Yes, and don't ask again",
            "edit to",
            "?",
            "9",
            ".",
        ];
        for _ in 0..5000 {
            let n = rnd() % 60;
            let text: String = (0..n)
                .map(|_| bits[(rnd() % bits.len() as u64) as usize])
                .collect();
            if let Some(p) = parse_prompt(&text) {
                assert!(p.selected < p.options.len());
                for c in [Choice::Approve, Choice::Always, Choice::Deny] {
                    if let Some(i) = p.index_for(c) {
                        assert!(i < p.options.len());
                        let _ = p.keys_for(i, rnd() & 1 == 0);
                    }
                }
            }
            let _ = describe_request(&text);
        }
    }

    #[test]
    fn not_prompts() {
        assert!(parse_prompt(&screen("working")).is_none());
        assert!(parse_prompt("").is_none());
        assert!(parse_prompt("❯ only one").is_none());
    }
}
