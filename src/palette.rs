//! Command palette (Ctrl-a :): fuzzy pick an action, or type a voice style
//! command such as "in account two approve".

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Same as Ctrl-a followed by this key.
    Key(char),
    Broadcast,
    VoicePushToTalk,
    VoiceToggleWake,
    VoiceStop,
    VoiceTrain,
    Layout(&'static str),
    HidePane,
    DuplicateTab,
    StopAllLoops,
    FreeNow,
    AssistantNew,
    OpenMic,
    ShowHidden,
    VoicePreview,
    StopTalking,
    AccountMenu,
    ToggleTrust,
    /// Permission mode for the focused pane's account.
    Perm(&'static str),
}

pub const ACTIONS: &[(&str, Action)] = &[
    ("new tab", Action::Key('t')),
    ("restart to update (or check for updates)", Action::Key('N')),
    ("close tab", Action::Key('w')),
    ("next tab", Action::Key('n')),
    ("previous tab", Action::Key('p')),
    ("overview of all tabs", Action::Key('o')),
    ("dashboard and usage", Action::Key('d')),
    ("sessions", Action::Key('H')),
    (
        "live map: every session as an animated diagram",
        Action::Key('G'),
    ),
    ("zoom pane", Action::Key('z')),
    ("restart tab (resume)", Action::Key('r')),
    ("stop tab", Action::Key('x')),
    ("log in account", Action::Key('I')),
    ("switch pane to next account", Action::Key('a')),
    ("add account", Action::Key('A')),
    ("refresh usage", Action::Key('u')),
    ("broadcast a prompt", Action::Broadcast),
    ("voice: push to talk", Action::VoicePushToTalk),
    ("voice: toggle wake word listening", Action::VoiceToggleWake),
    ("voice: turn off", Action::VoiceStop),
    ("voice: open mic (no wake word)", Action::OpenMic),
    ("voice: log of the last utterances", Action::Key('V')),
    ("voice: train the wake word", Action::VoiceTrain),
    ("voice: preview the talk back voice", Action::VoicePreview),
    ("voice: stop talking", Action::StopTalking),
    ("approvals queue", Action::Key('y')),
    ("loops: scheduled prompts in running tabs", Action::Key('@')),
    ("stop all loops", Action::StopAllLoops),
    ("memory saver on / off", Action::Key('Z')),
    ("assistant: open the conversation panel", Action::Key('.')),
    ("assistant: new conversation", Action::AssistantNew),
    (
        "free memory now (pause idle background tabs)",
        Action::FreeNow,
    ),
    (
        "auto trust folders: on / off for this account",
        Action::ToggleTrust,
    ),
    (
        "permissions: bypass (skip all checks)",
        Action::Perm("bypass"),
    ),
    (
        "permissions: default (claude decides)",
        Action::Perm("default"),
    ),
    ("permissions: auto", Action::Perm("auto")),
    ("permissions: accept edits", Action::Perm("accept-edits")),
    ("permissions: plan", Action::Perm("plan")),
    (
        "permissions: manual (ask every time)",
        Action::Perm("manual"),
    ),
    ("tab list: collapse or expand", Action::Key('s')),
    ("tab list position: top / left / right", Action::Key('S')),
    ("rename tab", Action::Key('R')),
    ("show or hide account emails", Action::Key('e')),
    (
        "layout: next (auto, grid, columns, rows, focus)",
        Action::Key('L'),
    ),
    ("layout: auto", Action::Layout("auto")),
    ("layout: grid", Action::Layout("grid")),
    ("layout: columns", Action::Layout("columns")),
    ("layout: rows", Action::Layout("rows")),
    ("layout: focus", Action::Layout("focus")),
    ("next page of panes", Action::Key(']')),
    ("previous page of panes", Action::Key('[')),
    (
        "split pane (another pane for this account)",
        Action::Key('|'),
    ),
    ("hide this pane (keeps running)", Action::HidePane),
    ("show hidden panes", Action::ShowHidden),
    ("go to pane number...", Action::Key('\'')),
    ("privacy mode: hide emails everywhere", Action::Key('E')),
    ("settings", Action::Key(',')),
    (
        "account menu (switch, log in, log out, tab position)",
        Action::AccountMenu,
    ),
    (
        "release the mouse to the terminal (select across panes)",
        Action::Key('M'),
    ),
    ("copy the selected text", Action::Key('c')),
    (
        "move this tab to another account (continue the conversation there)",
        Action::Key('m'),
    ),
    (
        "duplicate this tab to another account",
        Action::DuplicateTab,
    ),
    ("undo the last tab move", Action::Key('U')),
    ("take the tour", Action::Key('T')),
    ("help", Action::Key('?')),
    ("quit", Action::Key('q')),
];

/// Subsequence match score (lower is better), None if no match.
pub fn fuzzy_score(query: &str, item: &str) -> Option<usize> {
    let q: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if q.is_empty() {
        return Some(0);
    }
    let item_l = item.to_lowercase();
    if item_l.contains(&query.to_lowercase()) {
        return Some(item_l.find(&query.to_lowercase()).unwrap_or(0));
    }
    let mut qi = 0;
    let mut gaps = 0;
    let mut last: Option<usize> = None;
    for (i, c) in item_l.chars().enumerate() {
        if qi < q.len() && c == q[qi] {
            if let Some(l) = last {
                gaps += i - l - 1;
            }
            last = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then_some(100 + gaps)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Palette {
    pub input: String,
    pub sel: usize,
}

impl Palette {
    pub fn matches(&self) -> Vec<(&'static str, Action)> {
        let mut v: Vec<(usize, &'static str, Action)> = ACTIONS
            .iter()
            .filter_map(|(n, a)| fuzzy_score(&self.input, n).map(|s| (s, *n, *a)))
            .collect();
        v.sort_by_key(|(s, _, _)| *s);
        v.into_iter().map(|(_, n, a)| (n, a)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert_eq!(fuzzy_score("tab", "new tab"), Some(4));
        assert!(fuzzy_score("ntb", "new tab").is_some());
        assert!(fuzzy_score("xyz", "new tab").is_none());
        let p = Palette {
            input: "brod".into(),
            sel: 0,
        };
        assert_eq!(p.matches()[0].1, Action::Broadcast);
        let p = Palette {
            input: "next".into(),
            sel: 0,
        };
        assert_eq!(p.matches()[0].0, "next tab");
    }
}
