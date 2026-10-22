//! The demo agent's Codex, Cursor, Antigravity and OpenCode screens,
//! drawn after the real captures in tests/fixtures/screens (codex
//! 0.153.4, cursor-agent 2026.10.19, agy 1.4.4, opencode 2.0.32): the
//! banner, the conversation, the working line, the composer and the
//! approval prompts with the keys each CLI really takes, so GodTerm's
//! own detection (prompt_cli) reads them as it reads the real CLIs.
//! OpenCode's approval prompt was never captured, so its demo never asks.

use super::*;
use crossterm::event::KeyCode;

const SLATE: &str = "\x1b[38;2;150;165;190m";
const SAND: &str = "\x1b[38;2;200;172;128m";
const SAGE: &str = "\x1b[38;2;140;172;128m";
const ROSE: &str = "\x1b[38;2;196;136;136m";
/// Cursor's composer box: half blocks in its fill color around a filled row.
const BOX_FG: &str = "\x1b[38;2;46;47;50m";
const BOX_BG: &str = "\x1b[48;2;46;47;50m";

/// Cut plain text to `w` columns.
fn cut(s: &str, w: usize) -> String {
    fit(s, w)
}

/// `left` and `right` on one row `w` wide (right aligned when it fits).
fn spread(left: &str, right: &str, w: usize) -> (String, String, usize) {
    let l = cut(left, w);
    let room = w.saturating_sub(width(&l));
    if width(right) + 2 > room {
        return (l, String::new(), 0);
    }
    let gap = room - width(right);
    (l, right.to_string(), gap)
}

/// The model each CLI names (the scenario's when it sets one).
pub(super) fn model(f: Flavor, sc: &Scenario) -> String {
    if !sc.model.is_empty() {
        return sc.model.clone();
    }
    match f {
        Flavor::Codex => "gpt-6-astra high",
        Flavor::Cursor => "Auto",
        Flavor::Antigravity => "Gemini 3.8 Flash",
        Flavor::OpenCode => "Qwen3.6 27B · LM Studio (local)",
        Flavor::Grok => "grok-4.7-build-fast",
        Flavor::Claude => "Opus 5.5",
    }
    .to_string()
}

pub(super) fn version_line(f: Flavor) -> &'static str {
    match f {
        Flavor::Claude => "2.1.291 (Claude Code)",
        Flavor::Grok => "1.0.45",
        Flavor::Codex => "codex-cli 0.163.0",
        Flavor::Cursor => "2026.10.19-6bbf2c4",
        Flavor::Antigravity => "1.4.4",
        Flavor::OpenCode => "2.1.2",
    }
}

/// What the CLI prints when it quits (GodTerm learns the session id from
/// it, see `prompt_cli::exit_session_id`).
pub(super) fn farewell(f: Flavor, sid: &str) -> String {
    match f {
        Flavor::Claude => format!("Resume this session with:\n  claude --resume {sid}"),
        Flavor::Grok => format!("Resume this session with:\n  grok --resume {sid}"),
        Flavor::Codex => format!("To continue this session, run:\n  codex resume {sid}"),
        Flavor::Cursor => format!("  To resume this session: agent --resume={sid}"),
        Flavor::Antigravity => {
            format!("Resume with -c (or command below):\nagy --conversation={sid}")
        }
        Flavor::OpenCode => format!("  Session\n  Continue  opencode -s {sid}"),
    }
}

/// Whether the CLI asks before an edit / a command in its default mode.
pub(super) fn asks_for_edits(f: Flavor) -> bool {
    matches!(
        f,
        Flavor::Claude | Flavor::Grok | Flavor::Codex | Flavor::Antigravity
    )
}

pub(super) fn asks_for_commands(f: Flavor) -> bool {
    f != Flavor::OpenCode
}

fn first_word(cmd: &str) -> &str {
    cmd.split_whitespace().next().unwrap_or("these")
}

/// The command approval each CLI shows (wording from the captures).
pub(super) fn command_ask(f: Flavor, cmd: &str, desc: &str) -> Ask {
    let first = first_word(cmd);
    let (question, options, body): (&str, Vec<String>, Vec<String>) = match f {
        Flavor::Codex => (
            "Would you like to run the following command?",
            vec![
                "Yes, proceed (y)".into(),
                format!("Yes, and don't ask again for commands that start with `{first}` (p)"),
                "No, and tell Codex what to do differently (esc)".into(),
            ],
            vec![cmd.to_string(), desc.to_string()],
        ),
        Flavor::Cursor => (
            "Run this command?",
            vec![
                "Run (once) (y)".into(),
                format!("Add Shell({first}) to allowlist? (tab)"),
                "Run Everything (shift+tab)".into(),
                "Skip & tell the agent what to do instead (esc or n)".into(),
            ],
            vec![cmd.to_string(), format!("Not in allowlist: {first}")],
        ),
        _ => (
            "Run this command?",
            vec![
                "Yes, run command".into(),
                format!(
                    "Yes, and always allow in this conversation for commands that start with '{first}'"
                ),
                format!(
                    "Yes, and always allow for commands that start with '{first}' (Persist to settings.json)"
                ),
                "No, cancel".into(),
            ],
            vec![cmd.to_string()],
        ),
    };
    Ask {
        title: "Command".into(),
        body,
        diff: vec![],
        question: question.into(),
        options,
        sel: 0,
    }
}

/// An edit (or a new file) approval: Codex and Antigravity ask.
pub(super) fn edit_ask(f: Flavor, path: &str, diff: Vec<(char, u32, String)>, create: bool) -> Ask {
    match f {
        Flavor::Codex => Ask {
            title: "Edit".into(),
            body: vec![path.to_string()],
            diff,
            question: "Would you like to make the following edits?".into(),
            options: vec![
                "Yes, proceed (y)".into(),
                "Yes, and don't ask again for these files (a)".into(),
                "No, and tell Codex what to do differently (esc)".into(),
            ],
            sel: 0,
        },
        _ => Ask {
            title: if create { "Create file" } else { "Edit file" }.into(),
            body: vec![format!("{} {path}", if create { "create" } else { "edit" })],
            diff,
            question: if create {
                "Create this file?"
            } else {
                "Make this edit?"
            }
            .into(),
            options: vec![
                if create {
                    "Yes, create file"
                } else {
                    "Yes, make edit"
                }
                .into(),
                "Yes, and always allow edits in this conversation".into(),
                "No, cancel".into(),
            ],
            sel: 0,
        },
    }
}

/// The option a key picks, from the key its label names: "(y)", "(tab)",
/// "(shift+tab)", "(esc or n)" (Esc itself picks the last one elsewhere).
pub(super) fn hotkey_index(a: &Ask, code: KeyCode) -> Option<usize> {
    a.options.iter().position(|o| {
        let Some(open) = o.rfind(" (") else {
            return false;
        };
        let Some(inner) = o[open + 2..].strip_suffix(')') else {
            return false;
        };
        match (inner, code) {
            ("tab", KeyCode::Tab) => true,
            ("shift+tab", KeyCode::BackTab) => true,
            ("esc or n", KeyCode::Char('n')) => true,
            (k, KeyCode::Char(c)) => {
                k.chars().count() == 1 && k.starts_with(c) && c.is_ascii_alphabetic()
            }
            _ => false,
        }
    })
}

// ---------------------------------------------------------------------
// Drawing

fn spin(since: Instant, frames: &[&'static str], ms: u128) -> &'static str {
    frames[(since.elapsed().as_millis() / ms) as usize % frames.len()]
}

const BRAILLE: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
const BRAILLE2: [&str; 6] = ["⠘⠣", "⠰⠆", "⠤⠇", "⠆⠋", "⠃⠙", "⠉⠜"];

/// The top of the screen: the banner (sized to the width), OpenCode's
/// session header.
pub(super) fn head(s: &Screen, w: usize) -> Vec<String> {
    match s.flavor {
        Flavor::Claude | Flavor::Grok => s.banner.clone(),
        Flavor::Codex => {
            let bw = w.min(60).max(24);
            let inner = bw - 2;
            let row = |text: &str, styled: String| {
                let t = cut(text, inner - 1);
                let pad = inner.saturating_sub(1 + width(&t));
                // `styled` is `text` with colors; cut ones fall back to plain.
                let body = if width(text) <= inner - 1 { styled } else { t };
                format!("{GRAY}│{RESET} {body}{}{GRAY}│{RESET}", " ".repeat(pad))
            };
            let model_l = format!("model:     {}   /model to change", s.model);
            let dir_l = format!("directory: {}", s.shown);
            vec![
                format!("{GRAY}╭{}╮{RESET}", "─".repeat(inner)),
                row(
                    ">_ OpenAI Codex (v0.163.0)",
                    format!("{BOLD}>_{RESET} {BOLD}OpenAI Codex{RESET} {GRAY}(v0.163.0){RESET}"),
                ),
                row("", String::new()),
                row(
                    &model_l,
                    format!(
                        "{GRAY}model:{RESET}     {}   {GRAY}/model to change{RESET}",
                        s.model
                    ),
                ),
                row(&dir_l, format!("{GRAY}directory:{RESET} {}", s.shown)),
                format!("{GRAY}╰{}╯{RESET}", "─".repeat(inner)),
                String::new(),
                format!(
                    "  {GRAY}{}{RESET}",
                    cut(
                        "Tip: Use /mcp to list configured MCP tools.",
                        w.saturating_sub(2)
                    )
                ),
                String::new(),
            ]
        }
        Flavor::Cursor => vec![
            String::new(),
            format!("  {BOLD}Cursor Agent{RESET}"),
            format!("  {GRAY}v2026.10.19-6bbf2c4{RESET}"),
            String::new(),
        ],
        Flavor::Antigravity => {
            let logo = [
                "      ▄▀▀▄      ",
                "     ▀▀▀▀▀▀     ",
                "    ▀▀▀▀▀▀▀▀    ",
                "   ▄▀▀    ▀▀▄   ",
                "  ▄▀▀      ▀▀▄  ",
            ];
            let info = [
                format!("{BOLD}Antigravity CLI{RESET} 1.4.4"),
                format!("{GRAY}alex@example.com (Antigravity Starter Quota){RESET}"),
                format!("{} (High)", s.model),
                format!("{GRAY}{}{RESET}", s.shown),
                String::new(),
            ];
            let plain = [
                "Antigravity CLI 1.4.4".to_string(),
                "alex@example.com (Antigravity Starter Quota)".to_string(),
                format!("{} (High)", s.model),
                s.shown.clone(),
                String::new(),
            ];
            let mut out = vec![String::new()];
            for i in 0..5 {
                let room = w.saturating_sub(18);
                let text = if width(&plain[i]) <= room {
                    info[i].clone()
                } else {
                    cut(&plain[i], room)
                };
                out.push(format!("{SLATE}{}{RESET}  {text}", logo[i]));
            }
            out.push(String::new());
            out
        }
        Flavor::OpenCode => {
            let spinner = match &s.mode {
                Mode::Working { since, .. } => spin(*since, &BRAILLE, 100),
                _ => " ",
            };
            let title = cut(&s.title, w.saturating_sub(8));
            let pad = w.saturating_sub(4 + width(&title) + 2);
            vec![
                format!(
                    " {SLATE}{spinner}{RESET} {BOLD}{title}{RESET}{}{GRAY}+{RESET}",
                    " ".repeat(pad)
                ),
                String::new(),
            ]
        }
    }
}

/// A result as these CLIs word it: "+3 -1" for an edit, "84 lines" for
/// a new file (Claude's own wording stays in the transcript).
fn tidy(t: &str) -> String {
    let num = |s: &str| s.split_whitespace().next().unwrap_or("0").to_string();
    if let Some(rest) = t.strip_prefix("Updated ") {
        if let Some((_, counts)) = rest.rsplit_once(" with ") {
            if let Some((a, r)) = counts.split_once(" and ") {
                return format!("+{} -{}", num(a), num(r));
            }
        }
    }
    if let Some(rest) = t.strip_prefix("Wrote ") {
        if let Some((n, _)) = rest.split_once(" lines to ") {
            return format!("{n} lines");
        }
    }
    t.to_string()
}

/// One conversation line as the CLI draws it (an empty Vec: not shown).
pub(super) fn render_line(f: Flavor, l: &Line, w: usize) -> Vec<String> {
    let base = |name: &str| -> String {
        Path::new(name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_string())
    };
    let wrapped = |t: &str, first: &str, rest: &str, color: &str| -> Vec<String> {
        wrap(t, w.saturating_sub(width(&strip(first)) + 1))
            .into_iter()
            .enumerate()
            .map(|(i, s)| format!("{}{color}{s}{RESET}", if i == 0 { first } else { rest }))
            .collect()
    };
    let diff = |k: &char, n: &u32, code: &str, indent: &str| -> Vec<String> {
        let (c, sign) = match k {
            '+' => (SAGE, '+'),
            '-' => (ROSE, '-'),
            _ => (GRAY, ' '),
        };
        let body = cut(code, w.saturating_sub(width(indent) + 8));
        vec![format!(
            "{indent}{GRAY}{n:>4}{RESET} {c}{sign} {body}{RESET}"
        )]
    };
    let dim = |indent: &str, t: &str| -> Vec<String> {
        vec![format!(
            "{indent}{GRAY}{}{RESET}",
            cut(t, w.saturating_sub(width(&strip(indent)) + 1))
        )]
    };
    let tool = |indent: &str, mark: &str, head: String| -> Vec<String> {
        vec![format!(
            "{indent}{mark} {}",
            cut(
                &head,
                w.saturating_sub(width(indent) + width(&strip(mark)) + 2)
            )
        )]
    };
    match f {
        Flavor::Codex => match l {
            Line::Blank => vec![String::new()],
            Line::User(t) => wrapped(t, &format!("{GRAY}›{RESET} "), "  ", LIGHT),
            Line::Text(t) => wrapped(t, "• ", "  ", ""),
            Line::Tool(name, arg) => match name.as_str() {
                "Read" => vec![
                    format!("{GRAY}•{RESET} {BOLD}Explored{RESET}"),
                    format!(
                        "  {GRAY}└{RESET} Read {}",
                        cut(&base(arg), w.saturating_sub(10))
                    ),
                ],
                "Search" => vec![
                    format!("{GRAY}•{RESET} {BOLD}Explored{RESET}"),
                    format!(
                        "  {GRAY}└{RESET} Search {}",
                        cut(
                            arg.trim_start_matches("pattern: ").trim_matches('"'),
                            w.saturating_sub(12)
                        )
                    ),
                ],
                "Update" => tool("", &format!("{GRAY}•{RESET}"), format!("Edited {arg}")),
                "Write" => tool("", &format!("{GRAY}•{RESET}"), format!("Added {arg}")),
                "Bash" => tool("", &format!("{GRAY}•{RESET}"), format!("Ran {arg}")),
                "Update Todos" => vec![format!("{GRAY}•{RESET} Updated Plan")],
                n => tool("", &format!("{GRAY}•{RESET}"), format!("{n} {arg}")),
            },
            Line::Result(t) if t.starts_with("Read ") || t.starts_with("Found ") => vec![],
            Line::Result(t) if t.starts_with("Updated ") => vec![],
            Line::Result(t) => dim(&format!("  {GRAY}└{RESET} "), &tidy(t)),
            Line::More(t) => dim("    ", t),
            Line::Diff(k, n, c) => diff(k, n, c, "    "),
            Line::Todo(done, t) => dim(
                &format!("  {GRAY}└{RESET} "),
                &format!("{} {t}", if *done { "✔" } else { "□" }),
            ),
            Line::Dim(t) => dim("", t),
        },
        Flavor::Cursor => match l {
            Line::Blank => vec![String::new()],
            Line::User(t) => wrapped(t, "  ", "  ", &format!("{BOLD}{WHITE}")),
            Line::Text(t) => wrapped(t, "  ", "  ", ""),
            Line::Tool(name, arg) => match name.as_str() {
                "Bash" => tool("  ", &format!("{SAND}${RESET}"), arg.clone()),
                "Read" => tool("  ", &format!("{SLATE}⬢{RESET}"), format!("Read {arg}")),
                "Search" => tool(
                    "  ",
                    &format!("{SLATE}⬢{RESET}"),
                    format!("Grep {}", arg.trim_start_matches("pattern: ")),
                ),
                "Update" => tool("  ", &format!("{SLATE}⬢{RESET}"), format!("Edit {arg}")),
                "Write" => tool("  ", &format!("{SLATE}⬢{RESET}"), format!("Write {arg}")),
                "Update Todos" => vec![format!("  {SLATE}⬢{RESET} To-dos")],
                n => tool("  ", &format!("{SLATE}⬢{RESET}"), format!("{n} {arg}")),
            },
            Line::Result(t) | Line::More(t) => dim("    ", &tidy(t)),
            Line::Diff(k, n, c) => diff(k, n, c, "    "),
            Line::Todo(done, t) => dim("    ", &format!("{} {t}", if *done { "☑" } else { "☐" })),
            Line::Dim(t) => dim("", t),
        },
        Flavor::Antigravity => match l {
            Line::Blank => vec![String::new()],
            Line::User(t) => wrapped(t, &format!("{GRAY}>{RESET} "), "  ", LIGHT),
            Line::Text(t) => wrapped(t, "  ", "  ", ""),
            Line::Tool(name, arg) => {
                let dot = format!("{SAGE}●{RESET}");
                match name.as_str() {
                    "Bash" => tool("", &dot, format!("Ran ({arg}) (ctrl+o to expand)")),
                    "Read" => tool("", &dot, format!("Read ({arg})")),
                    "Search" => tool(
                        "",
                        &dot,
                        format!("Searched ({})", arg.trim_start_matches("pattern: ")),
                    ),
                    "Update" => tool("", &dot, format!("Edited ({arg})")),
                    "Write" => tool("", &dot, format!("Created ({arg})")),
                    "Update Todos" => vec![format!("{dot} Updated plan")],
                    n => tool("", &dot, format!("{n} ({arg})")),
                }
            }
            Line::Result(t) | Line::More(t) => dim("    ", &tidy(t)),
            Line::Diff(k, n, c) => diff(k, n, c, "    "),
            Line::Todo(done, t) => dim("    ", &format!("{} {t}", if *done { "✓" } else { "○" })),
            Line::Dim(t) => dim("", t),
        },
        // OpenCode
        _ => match l {
            Line::Blank => vec![String::new()],
            Line::User(t) => {
                let bar = format!("  {SAND}┃{RESET}");
                let mut v = vec![bar.clone()];
                v.extend(wrapped(t, &format!("{bar}  "), &format!("{bar}  "), ""));
                v.push(bar);
                v
            }
            Line::Text(t) => wrapped(t, "     ", "     ", ""),
            Line::Tool(name, arg) => match name.as_str() {
                "Bash" => tool("     ", &format!("{GRAY}${RESET}"), arg.clone()),
                "Read" => tool("     ", &format!("{GRAY}→{RESET}"), format!("Read {arg}")),
                "Search" => tool(
                    "     ",
                    &format!("{GRAY}✱{RESET}"),
                    format!("Grep {}", arg.trim_start_matches("pattern: ")),
                ),
                "Update" => tool("     ", &format!("{GRAY}←{RESET}"), format!("Edit {arg}")),
                "Write" => tool("     ", &format!("{GRAY}←{RESET}"), format!("Write {arg}")),
                "Update Todos" => vec![format!("     {GRAY}☐{RESET} Todos")],
                n => tool("     ", &format!("{GRAY}⚙{RESET}"), format!("{n} {arg}")),
            },
            Line::Result(t) | Line::More(t) => dim("       ", &tidy(t)),
            Line::Diff(k, n, c) => diff(k, n, c, "       "),
            Line::Todo(done, t) => {
                dim("       ", &format!("{} {t}", if *done { "■" } else { "□" }))
            }
            Line::Dim(t) => dim("", t),
        },
    }
}

fn rule(w: usize) -> String {
    format!("{GRAY}{}{RESET}", "─".repeat(w))
}

fn notice(s: &Screen) -> Option<String> {
    match &s.notice {
        Some((n, t)) if t.elapsed() < Duration::from_secs(3) => Some(n.trim().to_string()),
        _ => None,
    }
}

fn input_or(s: &Screen, placeholder: &str, room: usize) -> String {
    if s.input.is_empty() {
        format!("{GRAY}{}{RESET}", cut(placeholder, room))
    } else {
        format!("{}\x1b[7m \x1b[27m", cut(&s.input, room.saturating_sub(1)))
    }
}

/// Everything under the conversation: the working line, the composer, the
/// footer, or the approval prompt.
pub(super) fn footer(s: &Screen, w: usize) -> Vec<String> {
    let mut out = vec![];
    let working = match &s.mode {
        Mode::Working { since, .. } => Some(*since),
        _ => None,
    };
    match s.flavor {
        Flavor::Codex => {
            if let Mode::Asking(a) = &s.mode {
                out.push(String::new());
                out.push(format!("  {}", cut(&a.question, w.saturating_sub(3))));
                out.push(String::new());
                if a.title == "Command" {
                    if a.body.get(1).is_some_and(|d| !d.is_empty()) {
                        out.push(format!(
                            "  {GRAY}Reason:{RESET} {}",
                            cut(&a.body[1], w.saturating_sub(11))
                        ));
                        out.push(String::new());
                    }
                    out.push(format!("  $ {}", cut(&a.body[0], w.saturating_sub(5))));
                } else {
                    out.push(format!(
                        "  {}",
                        cut(
                            a.body.first().map(String::as_str).unwrap_or(""),
                            w.saturating_sub(3)
                        )
                    ));
                    for (k, n, c) in a.diff.iter().take(6) {
                        out.extend(render_line(
                            Flavor::Codex,
                            &Line::Diff(*k, *n, c.clone()),
                            w,
                        ));
                    }
                }
                out.push(String::new());
                for (i, o) in a.options.iter().enumerate() {
                    let label = cut(&format!("{}. {o}", i + 1), w.saturating_sub(3));
                    out.push(if i == a.sel {
                        format!("{SLATE}› {label}{RESET}")
                    } else {
                        format!("  {label}")
                    });
                }
                out.push(String::new());
                out.push(format!(
                    "  {GRAY}{}{RESET}",
                    cut(
                        "Press enter to confirm or esc to cancel",
                        w.saturating_sub(3)
                    )
                ));
                return out;
            }
            if let Some(since) = working {
                out.push(String::new());
                let dot = spin(since, &["•", "◦"], 500);
                out.push(format!(
                    "{dot} {BOLD}Working{RESET} {GRAY}{}{RESET}",
                    cut(
                        &format!("({}s • esc to interrupt)", since.elapsed().as_secs()),
                        w.saturating_sub(11)
                    )
                ));
            }
            out.push(String::new());
            out.push(format!(
                "{GRAY}›{RESET} {}",
                input_or(s, "Ask Codex to do anything", w.saturating_sub(3))
            ));
            out.push(String::new());
            let foot = notice(s).unwrap_or_else(|| format!("{} · {}", s.model, s.shown));
            out.push(format!(
                "  {GRAY}{}{RESET}",
                cut(&foot, w.saturating_sub(3))
            ));
        }
        Flavor::Cursor => {
            if let Mode::Asking(a) = &s.mode {
                out.push(String::new());
                out.push(rule(w));
                out.push(format!(
                    " {SAND}${RESET}  {}",
                    cut(&format!("{} in .", a.body[0]), w.saturating_sub(5))
                ));
                out.push(String::new());
                out.push(format!(
                    " {BOLD}{}{RESET}",
                    cut(&a.question, w.saturating_sub(2))
                ));
                if let Some(b) = a.body.get(1) {
                    out.push(format!(" {GRAY}{}{RESET}", cut(b, w.saturating_sub(2))));
                }
                for (i, o) in a.options.iter().enumerate() {
                    let label = cut(o, w.saturating_sub(5));
                    out.push(if i == a.sel {
                        format!("  {SLATE}→ {label}{RESET}")
                    } else {
                        format!("    {label}")
                    });
                }
                return out;
            }
            if let Some(since) = working {
                out.push(format!(
                    " {SLATE}{}{RESET} Working",
                    spin(since, &BRAILLE2, 120)
                ));
            }
            out.push(format!(
                " {BOX_FG}{}{RESET}",
                "▄".repeat(w.saturating_sub(2))
            ));
            let place = if working.is_some() {
                "Add a follow-up"
            } else {
                "Plan, search, build anything"
            };
            let right = if working.is_some() {
                "ctrl+c to stop"
            } else {
                ""
            };
            let room = w.saturating_sub(6 + width(right) + 2);
            let left = input_or(s, place, room);
            let plain = if s.input.is_empty() {
                cut(place, room)
            } else {
                format!("{} ", cut(&s.input, room.saturating_sub(1)))
            };
            let gap = w.saturating_sub(4 + width(&plain) + width(right) + 1);
            // (Colors reset inside `left`: the fill is set again after it.)
            let left = left.replace(RESET, &format!("{RESET}{BOX_BG}"));
            out.push(format!(
                " {BOX_BG} → {left}{BOX_BG}{}{GRAY}{right}{RESET}",
                " ".repeat(gap)
            ));
            out.push(format!(
                " {BOX_FG}{}{RESET}",
                "▀".repeat(w.saturating_sub(2))
            ));
            out.push(format!(
                "  {GRAY}{}{RESET}",
                cut(
                    &notice(s).unwrap_or_else(|| s.model.clone()),
                    w.saturating_sub(3)
                )
            ));
            out.push(format!(
                "  {GRAY}{}{RESET}",
                cut(&s.shown, w.saturating_sub(3))
            ));
        }
        Flavor::Antigravity => {
            let right = format!("{} · high", s.model);
            let bottom = |left: &str| {
                let (l, r, gap) = spread(left, &right, w.saturating_sub(1));
                format!("{GRAY}{l}{}{r}{RESET}", " ".repeat(gap))
            };
            if let Mode::Asking(a) = &s.mode {
                out.push(String::new());
                out.push(format!("{BOLD}{}{RESET}", a.title));
                out.push(rule(w));
                out.push(String::new());
                out.push("Requesting permission for:".into());
                out.push(format!(
                    "   {}",
                    cut(
                        a.body.first().map(String::as_str).unwrap_or(""),
                        w.saturating_sub(4)
                    )
                ));
                for (k, n, c) in a.diff.iter().take(6) {
                    out.extend(render_line(
                        Flavor::Antigravity,
                        &Line::Diff(*k, *n, c.clone()),
                        w,
                    ));
                }
                out.push(String::new());
                out.push(cut(&a.question, w.saturating_sub(1)));
                for (i, o) in a.options.iter().enumerate() {
                    let label = cut(&format!("{}. {o}", i + 1), w.saturating_sub(3));
                    out.push(if i == a.sel {
                        format!("{SLATE}> {label}{RESET}")
                    } else {
                        format!("  {label}")
                    });
                }
                out.push(String::new());
                out.push(format!(
                    "  {GRAY}{}{RESET}",
                    cut(
                        "↑/↓ Navigate · tab Amend · ctrl+g edit/expand command",
                        w.saturating_sub(3)
                    )
                ));
                out.push(bottom("esc to cancel"));
                return out;
            }
            if let Some(since) = working {
                out.push(format!(
                    "{SLATE}{}{RESET}  Generating...",
                    spin(since, &BRAILLE, 100)
                ));
            }
            out.push(rule(w));
            out.push(format!(
                "> {}",
                if s.input.is_empty() {
                    String::new()
                } else {
                    input_or(s, "", w.saturating_sub(3))
                }
            ));
            out.push(rule(w));
            let left = notice(s).unwrap_or_else(|| {
                if working.is_some() {
                    "esc to cancel"
                } else {
                    "? for shortcuts"
                }
                .to_string()
            });
            out.push(bottom(&left));
        }
        _ => {
            // OpenCode
            let bar = format!("  {SAND}┃{RESET}");
            out.push(String::new());
            out.push(bar.clone());
            out.push(format!(
                "{bar}  {}",
                input_or(s, "Ask anything… \"Fix broken tests\"", w.saturating_sub(6))
            ));
            out.push(bar.clone());
            out.push(format!(
                "{bar}  {}",
                cut(&format!("Build · {}", s.model), w.saturating_sub(6))
            ));
            out.push(format!(
                "  {SAND}╹{GRAY}{}{RESET}",
                "▀".repeat(w.saturating_sub(4))
            ));
            let right = "shift+tab agents  ctrl+p commands";
            let left = match (working, notice(s)) {
                (_, Some(n)) => n,
                (Some(since), None) => {
                    let i = (since.elapsed().as_millis() / 120) as usize % 8;
                    let dots: String = (0..8).map(|k| if k == i { '■' } else { '⬝' }).collect();
                    format!(" {dots} esc interrupt")
                }
                (None, None) => format!(" {}", s.shown),
            };
            let (l, r, gap) = spread(&format!("  {left}"), right, w.saturating_sub(1));
            out.push(format!("{GRAY}{l}{}{r}{RESET}", " ".repeat(gap)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::Activity;
    use crate::prompt::Choice;
    use crate::prompt_cli::{activity_for, describe_request_for, parse_prompt_for};

    fn screen(f: Flavor) -> Screen {
        let sc = Scenario {
            title: "Rate limit the gateway".into(),
            history: vec![Task {
                prompt: "add a token bucket".into(),
                steps: vec![],
                summary: "Added a token bucket per API key.".into(),
            }],
            ..Default::default()
        };
        let mut s = Screen::new(f, &sc, Path::new("/x/code/api-gateway"));
        s.lines.push(Line::User("add rate limit headers".into()));
        s.lines.push(Line::Blank);
        s.lines
            .push(Line::Tool("Read".into(), "src/limit.rs".into()));
        s.lines.push(Line::Result("Read 88 lines".into()));
        s.lines
            .push(Line::Tool("Update".into(), "src/limit.rs".into()));
        s.lines
            .push(Line::Diff('+', 12, "headers.insert(LIMIT, n);".into()));
        s.lines.push(Line::Tool("Bash".into(), "cargo test".into()));
        s.lines
            .push(Line::Result("test result: ok. 9 passed".into()));
        s.lines.push(Line::Text("Done.".into()));
        s
    }

    /// The frame as GodTerm's terminal reads it: plain lines.
    fn plain(s: &Screen, cols: u16, rows: u16) -> String {
        strip(&s.frame(cols, rows)).replace("\r\n", "\n")
    }

    fn working(s: &mut Screen) {
        s.mode = Mode::Working {
            verb: "Working".into(),
            since: Instant::now(),
            tokens: 0,
        };
    }

    #[test]
    fn every_cli_reads_as_ready_working_and_asking() {
        for f in [
            Flavor::Codex,
            Flavor::Cursor,
            Flavor::Antigravity,
            Flavor::OpenCode,
        ] {
            let h = f.harness();
            for cols in [60u16, 78, 120] {
                let mut s = screen(f);
                s.mode = Mode::Ready;
                assert_eq!(
                    activity_for(h, &plain(&s, cols, 40)),
                    Some(Activity::Ready),
                    "{f:?} ready {cols}"
                );
                working(&mut s);
                assert_eq!(
                    activity_for(h, &plain(&s, cols, 40)),
                    Some(Activity::Working),
                    "{f:?} working {cols}"
                );
                // No line is wider than the terminal (it would wrap).
                for l in plain(&s, cols, 40).lines() {
                    assert!(width(l) <= cols as usize, "{f:?} {cols}: {l:?}");
                }
                if !asks_for_commands(f) {
                    continue;
                }
                s.mode = Mode::Asking(command_ask(
                    f,
                    "npm install @acme/tooltip",
                    "Add the tooltip package",
                ));
                let text = plain(&s, cols, 40);
                for l in text.lines() {
                    assert!(width(l) <= cols as usize, "{f:?} {cols}: {l:?}");
                }
                assert_eq!(
                    activity_for(h, &text),
                    Some(Activity::Permission),
                    "{f:?} ask {cols}\n{text}"
                );
                let p = parse_prompt_for(h, &text)
                    .unwrap_or_else(|| panic!("{f:?}: no prompt\n{text}"));
                let r = describe_request_for(h, &text).expect("a request");
                assert!(r.detail.contains("npm install"), "{f:?}: {r:?}");
                for c in [Choice::Approve, Choice::Always, Choice::Deny] {
                    assert!(
                        p.index_for(c).is_some(),
                        "{f:?} {cols}: no {c:?} in {:?}",
                        p.options
                    );
                }
            }
        }
    }

    /// The keys GodTerm sends for each choice pick that option in the demo.
    #[test]
    fn the_keys_godterm_sends_pick_the_same_option() {
        for f in [Flavor::Codex, Flavor::Cursor, Flavor::Antigravity] {
            let h = f.harness();
            let mut s = screen(f);
            let a = command_ask(f, "docker compose up -d redis", "Start Redis");
            s.mode = Mode::Asking(a.clone());
            let text = plain(&s, 100, 40);
            let p = parse_prompt_for(h, &text).unwrap();
            for (c, want) in [
                (Choice::Approve, 0usize),
                (Choice::Deny, a.options.len() - 1),
            ] {
                let i = p.index_for(c).unwrap();
                let keys = p.keys_for(i, false);
                let got = match keys.as_slice() {
                    [0x1b] => Some(a.options.len() - 1),
                    [b'\t'] => hotkey_index(&a, KeyCode::Tab),
                    [d @ b'1'..=b'9'] => Some((d - b'1') as usize),
                    [c] => hotkey_index(&a, KeyCode::Char(*c as char)),
                    [.., b'\r'] => Some(p.selected),
                    k => panic!("{f:?}: keys {k:?}"),
                };
                assert_eq!(got, Some(want), "{f:?} {c:?} keys {keys:?}");
            }
        }
        // Codex edits ask too, Cursor and OpenCode do not.
        assert!(asks_for_edits(Flavor::Codex) && !asks_for_edits(Flavor::Cursor));
        let mut s = screen(Flavor::Codex);
        s.mode = Mode::Asking(edit_ask(
            Flavor::Codex,
            "src/limit.rs",
            vec![('+', 3, "x".into())],
            false,
        ));
        let text = plain(&s, 90, 40);
        assert_eq!(
            activity_for(crate::harness::Harness::Codex, &text),
            Some(Activity::Permission),
            "{text}"
        );
        let mut s = screen(Flavor::Antigravity);
        s.mode = Mode::Asking(edit_ask(
            Flavor::Antigravity,
            "lib/cart.dart",
            vec![],
            false,
        ));
        let text = plain(&s, 90, 40);
        assert_eq!(
            activity_for(crate::harness::Harness::Antigravity, &text),
            Some(Activity::Permission),
            "{text}"
        );
    }

    #[test]
    fn results_read_like_these_clis() {
        assert_eq!(
            tidy("Updated src/a.rs with 3 additions and 1 removal"),
            "+3 -1"
        );
        assert_eq!(tidy("Wrote 84 lines to src/T.tsx"), "84 lines");
        assert_eq!(tidy("ok  cli-tools/cmd"), "ok  cli-tools/cmd");
    }

    #[test]
    fn programs_flavors_and_farewells() {
        for (name, f) in Flavor::BINS {
            assert_eq!(Flavor::of_program(name), Some(*f));
            assert_eq!(f.bin(), *name);
            assert_eq!(Flavor::of_harness(f.harness()), *f);
        }
        assert_eq!(Flavor::of_program("agent"), Some(Flavor::Cursor));
        assert_eq!(Flavor::of_program("godterm"), None);
        let sid = "0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee";
        for f in [
            Flavor::Codex,
            Flavor::Cursor,
            Flavor::Antigravity,
            Flavor::OpenCode,
        ] {
            assert_eq!(
                crate::prompt_cli::exit_session_id(f.harness(), &farewell(f, sid)).as_deref(),
                Some(sid),
                "{f:?}"
            );
        }
    }
}
