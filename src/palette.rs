#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
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
    HeadphoneTap,
    ShowHidden,
    VoicePreview,
    StopTalking,
    AccountMenu,
    ToggleTrust,

    CreateToken,

    PaneWidth(i8),

    Perm(&'static str),

    UpdateMode,

    UpdatesNow,

    VoiceOnboarding,

    VoicePhrases,

    Setup,

    BrowserOpenTab,

    GoTab(u64),

    FindAll,
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
    ("pane wider", Action::PaneWidth(1)),
    ("pane narrower", Action::PaneWidth(-1)),
    ("equal pane widths", Action::PaneWidth(0)),
    ("restart tab (resume)", Action::Key('r')),
    ("stop tab", Action::Key('x')),
    ("log in account", Action::Key('I')),
    (
        "create long-lived token (claude setup-token, kept in the keychain)",
        Action::CreateToken,
    ),
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
    (
        "voice: update mode on / off (keep me posted on active projects)",
        Action::UpdateMode,
    ),
    (
        "voice: what's the latest (an update roundup now)",
        Action::UpdatesNow,
    ),
    (
        "voice: replay voice onboarding (turn voice on, checks, a short spoken tour)",
        Action::VoiceOnboarding,
    ),
    (
        "voice: voice phrases (the onboarding's card of useful phrases)",
        Action::VoicePhrases,
    ),
    (
        "voice: headphone tap on or off (play/pause turns the mic on or off)",
        Action::HeadphoneTap,
    ),
    ("approvals queue", Action::Key('y')),
    ("find: search every tab's recent output", Action::FindAll),
    ("browser: Chrome profile for this tab", Action::Key('i')),
    (
        "browser: open this tab's Chrome profile",
        Action::BrowserOpenTab,
    ),
    ("loops: scheduled prompts in running tabs", Action::Key('@')),
    ("stop all loops", Action::StopAllLoops),
    ("memory saver on / off", Action::Key('Z')),
    ("assistant: open the conversation panel", Action::Key('.')),
    ("assistant: new conversation", Action::AssistantNew),
    ("assistant: wider panel", Action::Key('{')),
    ("assistant: narrower panel", Action::Key('}')),
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
    ("reopen the last closed tab", Action::Key('W')),
    ("pin or unpin this tab", Action::Key('P')),
    ("back to the grid (home)", Action::Key('g')),
    ("install agents and voice (setup and checks)", Action::Setup),
    ("take the tour", Action::Key('T')),
    ("help", Action::Key('?')),
    ("quit", Action::Key('q')),
];

pub const ALIASES: &[(&str, &str)] = &[
    ("new tab", "open start session project folder create"),
    (
        "add account",
        "new account connect sign up claude grok codex cursor antigravity opencode agent",
    ),
    ("log in account", "login sign in signin auth relogin"),
    ("switch pane to next account", "change account other"),
    (
        "account menu (switch, log in, log out, tab position)",
        "switch account logout sign out",
    ),
    ("rename tab", "name title label"),
    (
        "move this tab to another account (continue the conversation there)",
        "transfer send",
    ),
    ("undo the last tab move", "revert back"),
    ("close tab", "delete remove kill end"),
    (
        "reopen the last closed tab",
        "undo close restore bring back",
    ),
    ("quit", "exit leave close godterm"),
    ("help", "keys shortcuts keyboard commands what can i do"),
    ("take the tour", "intro guide tutorial walkthrough"),
    ("settings", "preferences options config configure"),
    (
        "install agents and voice (setup and checks)",
        "install missing doctor dependencies deps setup claude grok codex cursor",
    ),
    ("voice: push to talk", "speak mic microphone talk"),
    ("voice: turn off", "mute disable"),
    ("zoom pane", "fullscreen maximize bigger"),
    ("approvals queue", "permission waiting approve allow"),
    ("sessions", "history resume past old conversations"),
    ("dashboard and usage", "limits quota left plan"),
    (
        "restart to update (or check for updates)",
        "upgrade version new release",
    ),
    (
        "loops: scheduled prompts in running tabs",
        "schedule cron timer repeat",
    ),
    ("broadcast a prompt", "send all everyone every"),
    ("copy the selected text", "clipboard"),
    ("go to pane number...", "jump"),
    ("privacy mode: hide emails everywhere", "hide email"),
    ("memory saver on / off", "ram"),
    ("back to the grid (home)", "home grid main panes"),
];

fn aliases(name: &str) -> &'static str {
    ALIASES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, a)| *a)
        .unwrap_or("")
}

fn words_match(query: &str, name: &str) -> bool {
    let hay: Vec<String> = name
        .split(|c: char| !c.is_alphanumeric())
        .chain(aliases(name).split(' '))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut any = false;
    for q in query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if matches!(q, "a" | "an" | "the" | "my" | "to" | "i") {
            continue;
        }
        any = true;
        if !hay.iter().any(|w| w.starts_with(q)) {
            return false;
        }
    }
    any
}

pub fn key_hint(a: Action) -> Option<String> {
    match a {
        Action::Key(' ') | Action::VoicePushToTalk => Some("Ctrl-a space".into()),
        Action::Key(c) => Some(format!("Ctrl-a {c}")),
        Action::Broadcast => Some("Ctrl-a b".into()),
        Action::VoiceToggleWake => Some("Ctrl-a v".into()),
        Action::VoicePhrases => Some("Ctrl-a J".into()),
        _ => None,
    }
}

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
    if words_match(query, item) {
        return Some(60);
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

#[cfg(test)]
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

const TAB_ITEMS: usize = 12;

pub fn items(app: &crate::app::App, pl: &Palette) -> Vec<(String, Action)> {
    let mut v: Vec<(usize, u8, String, Action)> = ACTIONS
        .iter()
        .filter_map(|(n, a)| fuzzy_score(&pl.input, n).map(|s| (s, 0, n.to_string(), *a)))
        .collect();
    if !pl.input.trim().is_empty() {
        let mut tabs = vec![];
        for slot in &app.panes {
            if slot.account.is_some_and(|a| app.account_closed(a)) {
                continue;
            }
            let acct = slot
                .account
                .map(|a| app.account_cfg(a).display().to_string())
                .unwrap_or_default();
            for tab in &slot.tabs {
                let name = tab.name();
                let folder = crate::paths::tilde(&tab.cwd);
                let state = if tab.is_running() {
                    crate::ui_chrome::short_state(tab.activity.label())
                } else {
                    "not started".to_string()
                };
                let label = format!("go to tab: {name} · {acct} · {state}");
                let score = [
                    fuzzy_score(&pl.input, &name),
                    fuzzy_score(&pl.input, &label),
                    fuzzy_score(&pl.input, &format!("{acct} {folder}")).map(|s| s + 50),
                ]
                .into_iter()
                .flatten()
                .min();
                if let Some(sc) = score {
                    tabs.push((sc, 1, label, Action::GoTab(tab.uid)));
                }
            }
        }
        tabs.sort_by_key(|t| t.0);
        v.extend(tabs.into_iter().take(TAB_ITEMS));
    }
    v.sort_by_key(|(s, kind, _, _)| (*s, *kind));
    v.into_iter().map(|(_, _, n, a)| (n, a)).collect()
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

    fn top(q: &str) -> &'static str {
        Palette {
            input: q.into(),
            sel: 0,
        }
        .matches()[0]
            .0
    }

    #[test]
    fn plain_words_find_actions() {
        assert_eq!(top("sign in"), "log in account");
        assert_eq!(top("login"), "log in account");
        assert_eq!(top("new account"), "add account");
        assert_eq!(top("connect codex"), "add account");
        assert_eq!(top("exit"), "quit");
        assert_eq!(top("shortcuts"), "help");
        assert_eq!(top("history"), "sessions");
        assert_eq!(
            top("install"),
            "install agents and voice (setup and checks)"
        );
        assert_eq!(top("reopen"), "reopen the last closed tab");
        assert_eq!(top("switch account"), "switch pane to next account");
        assert_eq!(top("rename"), "rename tab");
    }

    #[test]
    fn every_alias_names_an_action() {
        for (n, _) in ALIASES {
            assert!(ACTIONS.iter().any(|(a, _)| a == n), "{n}");
        }
    }

    #[test]
    fn key_hints() {
        assert_eq!(key_hint(Action::Key('t')).as_deref(), Some("Ctrl-a t"));
        assert_eq!(key_hint(Action::Broadcast).as_deref(), Some("Ctrl-a b"));
        assert_eq!(key_hint(Action::FreeNow), None);
    }
}
