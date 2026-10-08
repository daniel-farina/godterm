//! The menu bar's dropdown menus (Tabs, View, Voice, Settings), the way
//! macOS does them: click a title to open, hover across titles to switch,
//! click outside (consumed) or Esc to close, arrows and Enter, a first
//! letter jumps, submenus open on hover or Right, separators, checkmarks,
//! disabled items with a reason, shortcuts right aligned, never off screen.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

use crate::app::{App, Modal};
use crate::hits::UiAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuId {
    Tabs,
    View,
    Voice,
    Settings,
    // Submenus.
    TabPos,
    TabWidth,
    Layout,
    /// The Sessions view's Sort ▾ and its Group by ▸.
    SessSort,
    SessGroup,
    RecentlyClosed,
    PauseListening,
    AssistantAccount,
    Permissions,
    /// Tabs ▾ > Sort tabs ▸ (the focused pane's list).
    SortTabs,
    /// The assistant panel's ⋯ menu.
    AssistantMore,
    /// View ▾ > Closed accounts ▸.
    ClosedAccounts,
}

impl MenuId {
    pub const TOP: &'static [MenuId] =
        &[MenuId::Tabs, MenuId::View, MenuId::Voice, MenuId::Settings];
}

/// Commands only the menus need.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Duplicate,
    TabPos(u8),
    TabWidth(u8),
    Layout(u8),
    SessSort(u8),
    SessReverse,
    Reopen(u8),
    /// Pause listening for this many seconds (0: resume).
    Pause(u32),
    TabHistory,
    SessGroup(u8),
    AssistantAccount(Option<usize>),
    /// The assistant's model (alias or name).
    AssistantModel(String),
    AssistantProvider(String),
    OpenSetup,
    AssistantRemote,
    AssistantLogin,
    /// Open closed accounts (None: all of them).
    OpenAccount(Option<usize>),
    AssistantRules,
    AssistantPrompt,
    AssistantAdmin,
    AssistantDetails,
    /// The panel: docked, overlay, auto (app_talkback::PANEL_MODES).
    PanelMode(u8),
    SpeakTyped,
    AssistantSettings,
    ShowPath,
    Permission(u8),
    TrainWake,
    Tour,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Entry {
    pub label: String,
    /// The shortcut, shown right aligned ("C-a t").
    pub key: &'static str,
    pub action: Option<UiAction>,
    /// Some: a toggle or radio row (✓ or •).
    pub check: Option<bool>,
    pub radio: bool,
    /// Some: dimmed, with why on hover.
    pub disabled: Option<String>,
    pub sub: Option<MenuId>,
    pub sep: bool,
    /// More about the row, dim after its name (tone 1: stands out, 2: a
    /// warning); cut with … first when the menu is too narrow.
    pub info: Vec<(String, u8)>,
}

fn item(label: impl Into<String>, key: &'static str, action: UiAction) -> Entry {
    Entry {
        label: label.into(),
        key,
        action: Some(action),
        ..Default::default()
    }
}

fn sep() -> Entry {
    Entry {
        sep: true,
        ..Default::default()
    }
}

fn sub(label: &str, id: MenuId) -> Entry {
    Entry {
        label: label.into(),
        sub: Some(id),
        ..Default::default()
    }
}

pub fn entries(app: &App, id: MenuId) -> Vec<Entry> {
    let waiting = app.waiting_tabs().len();
    let hidden = app.hidden_count();
    let has_acct = app.panes.get(app.focus).and_then(|p| p.account).is_some();
    let no_acct = |mut e: Entry| {
        if !has_acct {
            e.disabled = Some("the focused pane has no account".into());
        }
        e
    };
    match id {
        MenuId::Tabs => vec![
            no_acct(item("New tab", "C-a t", UiAction::Key('t'))),
            item("Close tab", "C-a w", UiAction::Key('w')),
            {
                let mut e = item("Reopen closed tab", "C-a W", UiAction::ReopenClosed);
                if app.closed.is_empty() {
                    e.disabled = Some("nothing was closed recently".into());
                }
                e
            },
            {
                let mut e = sub("Recently closed", MenuId::RecentlyClosed);
                if app.closed.is_empty() {
                    e.disabled = Some("nothing was closed recently".into());
                }
                e
            },
            item("Rename", "C-a R", UiAction::Key('R')),
            sep(),
            no_acct(item("Move to...", "C-a m", UiAction::Key('m'))),
            no_acct(item("Duplicate to...", "", UiAction::Menu(Cmd::Duplicate))),
            item("Broadcast...", "C-a b", UiAction::Key('b')),
            sep(),
            {
                let mut e = item(format!("Hidden panes ({hidden})"), "", UiAction::ShowHidden);
                if hidden == 0 {
                    e.disabled = Some("no pane is hidden".into());
                }
                e
            },
            sub("Tab list position", MenuId::TabPos),
            sub("Tab list width", MenuId::TabWidth),
            sub("Sort tabs", MenuId::SortTabs),
            item("Pin / unpin tab", "C-a P", UiAction::Key('P')),
            item("Undo tab move", "C-a U", UiAction::Key('U')),
        ],
        MenuId::SortTabs => {
            let cur = app.panes.get(app.focus).map(|p| p.sort).unwrap_or_default();
            crate::tab_groups::SORTS
                .iter()
                .map(|s| Entry {
                    radio: true,
                    check: Some(cur == *s),
                    ..item(s.label(), "", UiAction::SortTabs(*s))
                })
                .collect()
        }
        MenuId::TabPos => {
            let cur = app.tab_pos(app.focus);
            [
                ("Left", crate::slot::TabPos::Left),
                ("Right", crate::slot::TabPos::Right),
                ("Top", crate::slot::TabPos::Top),
            ]
            .iter()
            .enumerate()
            .map(|(i, (l, p))| Entry {
                radio: true,
                check: Some(cur == *p),
                ..item(*l, "", UiAction::Menu(Cmd::TabPos(i as u8)))
            })
            .collect()
        }
        MenuId::TabWidth => {
            let cur = app.tab_list_width.as_deref().unwrap_or("normal");
            ["Narrow", "Normal", "Wide"]
                .iter()
                .enumerate()
                .map(|(i, l)| Entry {
                    radio: true,
                    check: Some(cur.eq_ignore_ascii_case(l)),
                    ..item(*l, "", UiAction::Menu(Cmd::TabWidth(i as u8)))
                })
                .collect()
        }
        MenuId::RecentlyClosed => app
            .closed
            .iter()
            .rev()
            .take(10)
            .enumerate()
            .map(|(i, c)| {
                let acct = app
                    .cfg
                    .accounts
                    .iter()
                    .find(|a| a.name == c.account)
                    .map(|a| a.display().to_string())
                    .unwrap_or_else(|| c.account.clone());
                let ago = crate::ui::ago_secs(chrono::Utc::now().timestamp() - c.closed_at);
                item(
                    format!("{} · {acct} · {ago}", crate::paths::middle(&c.label, 28)),
                    "",
                    UiAction::Menu(Cmd::Reopen(i as u8)),
                )
            })
            .collect(),
        MenuId::PauseListening => [
            (30u32, "30 s"),
            (60, "1 min"),
            (300, "5 min"),
            (900, "15 min"),
        ]
        .iter()
        .map(|(s, l)| item(*l, "", UiAction::Menu(Cmd::Pause(*s))))
        .collect(),
        MenuId::SessSort => {
            let mut v: Vec<Entry> = crate::sess_sort::KEYS
                .iter()
                .enumerate()
                .map(|(i, k)| Entry {
                    radio: true,
                    check: Some(app.sess_sort == *k),
                    ..item(
                        k.label(),
                        if i == 0 { "s" } else { "" },
                        UiAction::Menu(Cmd::SessSort(i as u8)),
                    )
                })
                .collect();
            v.push(sep());
            v.push(Entry {
                check: Some(app.sess_sort_rev),
                ..item("Reverse", "S", UiAction::Menu(Cmd::SessReverse))
            });
            v.push(sub("Group by", MenuId::SessGroup));
            v
        }
        MenuId::SessGroup => crate::sess_sort::GROUPS
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let l = match g {
                    crate::sess_sort::Group::None => "None",
                    crate::sess_sort::Group::Source => "Source",
                    crate::sess_sort::Group::Project => "Project",
                    crate::sess_sort::Group::Harness => "Harness",
                };
                Entry {
                    radio: true,
                    check: Some(app.sess_group == *g),
                    ..item(l, "", UiAction::Menu(Cmd::SessGroup(i as u8)))
                }
            })
            .collect(),
        MenuId::View => vec![
            item("Overview", "C-a o", UiAction::Key('o')),
            item("Dashboard", "C-a d", UiAction::Key('d')),
            item("Sessions", "C-a H", UiAction::Key('H')),
            item("Tab history", "", UiAction::Menu(Cmd::TabHistory)),
            item(
                format!("Loops ({})", app.loops.len()),
                "C-a @",
                UiAction::Key('@'),
            ),
            item(
                format!("Approvals ({waiting})"),
                "C-a y",
                UiAction::Key('y'),
            ),
            sep(),
            sub("Layout", MenuId::Layout),
            {
                let a = app.panes.get(app.focus).and_then(|p| p.account);
                let mut e = item(
                    match a {
                        Some(a) => {
                            format!("Close {} (stays logged in)", app.cfg.accounts[a].display())
                        }
                        None => "Close this account".into(),
                    },
                    "C-a C",
                    UiAction::Key('C'),
                );
                if a.is_none() {
                    e.disabled = Some("no account in this pane".into());
                }
                e
            },
            {
                let n = (0..app.cfg.accounts.len())
                    .filter(|a| app.account_closed(*a))
                    .count();
                let mut e = sub(&format!("Closed accounts ({n})"), MenuId::ClosedAccounts);
                if n == 0 {
                    e.disabled = Some("no account is closed".into());
                }
                e
            },
            item("Live map", "C-a G", UiAction::Key('G')),
            Entry {
                check: Some(app.zoom),
                ..item("Zoom the focused pane", "C-a z", UiAction::Key('z'))
            },
            {
                let mut e = item("Next page", "C-a ]", UiAction::Key(']'));
                if app.page_count() <= 1 {
                    e.disabled = Some("every pane fits on one page".into());
                }
                e
            },
            {
                let mut e = item("Previous page", "C-a [", UiAction::Key('['));
                if app.page_count() <= 1 {
                    e.disabled = Some("every pane fits on one page".into());
                }
                e
            },
            sep(),
            item("Learned rules", "", UiAction::OpenLearned),
            item("System prompt", "", UiAction::OpenPrompt),
        ],
        MenuId::Layout => crate::layout::MODES
            .iter()
            .enumerate()
            .map(|(i, m)| Entry {
                radio: true,
                check: Some(app.layout_name() == *m),
                ..item(
                    *m,
                    if i == 0 { "C-a L" } else { "" },
                    UiAction::Menu(Cmd::Layout(i as u8)),
                )
            })
            .collect(),
        MenuId::Voice => {
            let cur = app.voice_mode();
            let mut v: Vec<Entry> = ["Off", "Push to talk", "Wake word", "Open mic"]
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let mut e = Entry {
                        radio: true,
                        check: Some(cur == i as u8 && !app.voice.muted),
                        ..item(
                            *l,
                            if i == 1 {
                                "C-a space"
                            } else if i == 2 {
                                "C-a v"
                            } else {
                                ""
                            },
                            UiAction::VoiceMode(i as u8),
                        )
                    };
                    if app.voice.muted {
                        e.disabled = Some("the mic is muted (C-a X)".into());
                    }
                    e
                })
                .collect();
            v.push(Entry {
                check: Some(app.voice.muted),
                ..item("Mute the mic", "C-a X", UiAction::MuteToggle)
            });
            v.push(Entry {
                check: Some(app.cfg.voice.speaker_muted),
                ..item("Mute the speaker", "C-a O", UiAction::SpeakerToggle)
            });
            v.push(sub("Pause listening", MenuId::PauseListening));
            {
                let mut e = item("Resume listening", "", UiAction::Menu(Cmd::Pause(0)));
                if !app.paused() {
                    e.disabled = Some("listening is not paused".into());
                }
                v.push(e);
            }
            v.push(sep());
            v.push(Entry {
                check: Some(app.assistant.show),
                ..item("Assistant panel", "C-a .", UiAction::Key('.'))
            });
            v.push(sub("Assistant account", MenuId::AssistantAccount));
            v.push(item(
                "Train the wake word...",
                "",
                UiAction::Menu(Cmd::TrainWake),
            ));
            v.push(Entry {
                check: Some(app.voice.show_log),
                ..item("Voice log", "C-a V", UiAction::Key('V'))
            });
            v
        }
        MenuId::AssistantAccount => {
            use crate::app_provider::bucket_parts;
            let cur = app.cfg.assistant.account.clone();
            let prov = crate::providers::by_id(&app.cfg.assistant.provider);
            let dim = |t: String| vec![(t, 0u8)];
            let mut v: Vec<Entry> = crate::providers::PROVIDERS
                .iter()
                .map(|p| {
                    let info = match (p.harness, p.own_home) {
                        (Some(h), _) => {
                            let n = app.cfg.accounts.iter().filter(|a| a.harness() == h).count();
                            let mut t = format!("{n} account{}", if n == 1 { "" } else { "s" });
                            if let Some((b, l)) = app.best_account() {
                                if app.cfg.accounts[b].harness() == h {
                                    t.push_str(&format!(
                                        " · best: {} ({l:.0}%)",
                                        app.cfg.accounts[b].display()
                                    ));
                                }
                            }
                            dim(t)
                        }
                        (None, Some(home)) => dim(if crate::harness::grok::logged_in(&home()) {
                            "own login · signed in".into()
                        } else {
                            "own login · not signed in".into()
                        }),
                        (None, None) => vec![],
                    };
                    Entry {
                        radio: true,
                        check: Some(p.id == prov.id),
                        info,
                        ..item(
                            p.name,
                            "",
                            UiAction::Menu(Cmd::AssistantProvider(p.id.to_string())),
                        )
                    }
                })
                .collect();
            v.push(sep());
            let model = crate::providers::model_for(prov, &app.cfg.assistant);
            let models = |v: &mut Vec<Entry>| {
                for &(m, label, about) in prov.models {
                    v.push(Entry {
                        radio: true,
                        check: Some(model == m || (m == "claude-haiku-4-5" && model == "haiku")),
                        info: dim(about.to_string()),
                        ..item(
                            label,
                            "",
                            UiAction::Menu(Cmd::AssistantModel(m.to_string())),
                        )
                    });
                }
            };
            if let Some(home) = prov.own_home {
                let usage = match app.own_buckets(prov) {
                    Some(b) if !b.is_empty() => bucket_parts(&b),
                    Some(_) => dim("usage not reported".into()),
                    None => vec![],
                };
                if !crate::harness::grok::logged_in(&home()) {
                    v.push(item(
                        format!("Log in the assistant's {}...", prov.name),
                        "",
                        UiAction::Menu(Cmd::AssistantLogin),
                    ));
                } else {
                    v.push(Entry {
                        disabled: Some("its own login, not one of your accounts".into()),
                        info: usage,
                        ..item(
                            format!("{} (its own login)", prov.name),
                            "",
                            UiAction::Menu(Cmd::AssistantLogin),
                        )
                    });
                }
                v.push(sep());
                models(&mut v);
                return v;
            }
            let usage_of = |a: usize| -> Vec<(String, u8)> {
                match app.account_buckets(a) {
                    Some(b) if b.is_empty() => dim("usage not reported".into()),
                    Some(b) => {
                        let mut parts = bucket_parts(&b);
                        if let Some(k) = crate::providers::binding(&b)
                            .filter(|k| k.left_pct < crate::providers::LOW_PCT)
                        {
                            if let Some(t) = k.resets_at {
                                parts.push((
                                    format!(
                                        "  ⚠ {} resets {}",
                                        k.long,
                                        crate::app_provider::reset_word(t)
                                    ),
                                    2,
                                ));
                            }
                        }
                        parts
                    }
                    None => vec![],
                }
            };
            let best = app.best_account();
            v.push(Entry {
                radio: true,
                check: Some(cur == "best" || cur.is_empty()),
                info: match best {
                    Some((b, _)) => {
                        let mut p = dim(format!("now {} · ", app.cfg.accounts[b].display()));
                        p.extend(usage_of(b));
                        p
                    }
                    None => dim("none has usage left".into()),
                },
                ..item(
                    "The one with the most left",
                    "",
                    UiAction::Menu(Cmd::AssistantAccount(None)),
                )
            });
            for (i, a) in app.cfg.accounts.iter().enumerate() {
                let why = app.account_unusable(i, prov);
                let info = match &why {
                    Some(w) => dim(w.clone()),
                    None => usage_of(i),
                };
                v.push(Entry {
                    radio: true,
                    check: Some(cur == a.name),
                    info,
                    disabled: why,
                    ..item(
                        a.display(),
                        "",
                        UiAction::Menu(Cmd::AssistantAccount(Some(i))),
                    )
                });
            }
            v.push(sep());
            models(&mut v);
            v
        }
        MenuId::ClosedAccounts => {
            let mut v: Vec<Entry> = (0..app.cfg.accounts.len())
                .filter(|a| app.account_closed(*a))
                .map(|a| {
                    item(
                        format!("Open {}", app.cfg.accounts[a].display()),
                        "",
                        UiAction::Menu(Cmd::OpenAccount(Some(a))),
                    )
                })
                .collect();
            if v.len() > 1 {
                v.push(sep());
                v.push(item("Open all", "", UiAction::Menu(Cmd::OpenAccount(None))));
            }
            v
        }
        MenuId::AssistantMore => vec![
            item("New conversation", "", UiAction::AssistantNew),
            item(
                if app.assistant.history.is_some() {
                    "Back to the chat"
                } else {
                    "History"
                },
                "",
                UiAction::AssistantHistory,
            ),
            sub("Account and model", MenuId::AssistantAccount),
            {
                let mut e = Entry {
                    check: Some(app.assistant.remote.is_some()),
                    ..item("Remote Control", "", UiAction::Menu(Cmd::AssistantRemote))
                };
                if let Err(why) = app.remote_supported() {
                    e.disabled = Some(why);
                }
                e
            },
            sep(),
            item("Learned rules", "", UiAction::Menu(Cmd::AssistantRules)),
            item("System prompt", "", UiAction::Menu(Cmd::AssistantPrompt)),
            Entry {
                check: Some(app.assistant.show_admin),
                ..item("Admin actions", "", UiAction::Menu(Cmd::AssistantAdmin))
            },
            Entry {
                check: Some(app.assistant.show_details),
                ..item("Timing details", "", UiAction::Menu(Cmd::AssistantDetails))
            },
            sep(),
            Entry {
                check: Some(app.cfg.voice.speaker_muted),
                ..item("Mute the speaker", "C-a O", UiAction::SpeakerToggle)
            },
            Entry {
                check: Some(app.cfg.assistant.speak_typed),
                ..item(
                    "Speak replies to typed messages",
                    "",
                    UiAction::Menu(Cmd::SpeakTyped),
                )
            },
            Entry {
                check: Some(app.cfg.assistant.panel == "docked"),
                ..item("Panel: docked", "", UiAction::Menu(Cmd::PanelMode(0)))
            },
            Entry {
                check: Some(app.cfg.assistant.panel == "overlay"),
                ..item("Panel: floating", "", UiAction::Menu(Cmd::PanelMode(1)))
            },
            Entry {
                check: Some(app.cfg.assistant.panel == "auto"),
                ..item("Panel: auto", "", UiAction::Menu(Cmd::PanelMode(2)))
            },
            sep(),
            item(
                "Assistant settings",
                "",
                UiAction::Menu(Cmd::AssistantSettings),
            ),
            item("Close", "C-a .", UiAction::Key('.')),
        ],
        MenuId::Settings => vec![
            item("Settings...", "C-a ,", UiAction::Key(',')),
            sep(),
            Entry {
                check: Some(app.memory_saver_on()),
                ..item("Memory saver", "C-a Z", UiAction::Key('Z'))
            },
            Entry {
                check: Some(app.privacy()),
                ..item("Privacy mode", "C-a E", UiAction::Key('E'))
            },
            Entry {
                check: Some(app.show_email_global()),
                ..item("Show emails", "C-a e", UiAction::Key('e'))
            },
            Entry {
                check: Some(app.cfg.show_path),
                ..item("Show folders", "", UiAction::Menu(Cmd::ShowPath))
            },
            no_acct(sub("Permissions", MenuId::Permissions)),
            sep(),
            item("Setup", "", UiAction::Menu(Cmd::OpenSetup)),
            item("Doctor", "", UiAction::RunDoctor),
            if app.update.ready().is_some() {
                item("Restart to update", "C-a N", UiAction::Key('N'))
            } else {
                item("Check for updates", "C-a N", UiAction::UpdateCheck)
            },
            item("Help", "C-a ?", UiAction::Key('?')),
            item("Tour", "C-a T", UiAction::Menu(Cmd::Tour)),
            item("Quit", "C-a q", UiAction::Key('q')),
        ],
        MenuId::Permissions => {
            let cur = app
                .panes
                .get(app.focus)
                .and_then(|p| p.account)
                .map(|a| app.cfg.mode_for(a))
                .unwrap_or_default();
            crate::settings::MODES
                .iter()
                .enumerate()
                .map(|(i, m)| Entry {
                    radio: true,
                    check: Some(cur == *m),
                    ..item(*m, "", UiAction::Menu(Cmd::Permission(i as u8)))
                })
                .collect()
        }
    }
}

/// An open menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Open {
    pub id: MenuId,
    pub sel: Option<usize>,
    /// Where it hangs from (the title or chip that opened it).
    pub anchor: Option<(u16, u16)>,
    /// An open submenu and its selection.
    pub sub: Option<(MenuId, Option<usize>)>,
}

impl Open {
    pub fn new(id: MenuId, anchor: Option<(u16, u16)>) -> Open {
        Open {
            id,
            sel: None,
            anchor,
            sub: None,
        }
    }
}

fn usable(e: &Entry) -> bool {
    !e.sep && e.disabled.is_none()
}

/// Where the menu box goes: under the anchor, shifted left to fit, or
/// above it when there is no room below.
pub fn place(area: Rect, anchor: (u16, u16), w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = anchor.0.min(area.x + area.width - w);
    let below = anchor.1 + 1;
    let y = if below + h <= area.y + area.height {
        below
    } else {
        anchor.1.saturating_sub(h).max(area.y)
    };
    Rect::new(x, y, w, h)
}

/// Text width of a menu's rows.
pub fn width(es: &[Entry]) -> u16 {
    let lw = es
        .iter()
        .map(|e| e.label.chars().count())
        .max()
        .unwrap_or(4);
    let kw = es.iter().map(|e| e.key.chars().count()).max().unwrap_or(0);
    let iw = es.iter().map(info_width).max().unwrap_or(0);
    let iw = if iw > 0 { iw + 2 } else { 0 };
    (lw + iw + kw + 8) as u16
}

/// The info column's width of a row.
pub fn info_width(e: &Entry) -> usize {
    e.info
        .iter()
        .map(|(t, _)| unicode_width::UnicodeWidthStr::width(t.as_str()))
        .sum()
}

impl App {
    pub fn open_menu(&mut self, id: MenuId) {
        let anchor = self
            .last_hits
            .find(&UiAction::MenuOpen(id))
            .map(|r| (r.rect.x, r.rect.y));
        self.modal = Modal::Menu(Open::new(id, anchor));
    }

    pub fn menu_state(&self) -> Option<&Open> {
        match &self.modal {
            Modal::Menu(o) => Some(o),
            _ => None,
        }
    }

    /// Run a row's action (closes the menu first).
    pub fn menu_run(&mut self, e: &Entry) {
        let Some(a) = e.action.clone() else { return };
        self.modal = Modal::None;
        match a {
            UiAction::Menu(c) => self.menu_cmd(c),
            other => {
                let m = crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: 0,
                    row: 0,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                };
                self.click(other, false, &m);
            }
        }
    }

    pub fn menu_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Duplicate => {
                let (p, t) = (self.focus, self.panes[self.focus].active);
                self.open_move_picker(p, t, true);
            }
            Cmd::TabPos(i) => {
                let pos = [
                    crate::slot::TabPos::Left,
                    crate::slot::TabPos::Right,
                    crate::slot::TabPos::Top,
                ][i.min(2) as usize];
                let f = self.focus;
                self.set_tab_pos(f, Some(pos));
            }
            Cmd::TabWidth(i) => {
                self.set_tab_list_width(["narrow", "normal", "wide"][i.min(2) as usize])
            }
            Cmd::SessSort(i) => {
                self.sess_sort =
                    crate::sess_sort::KEYS[(i as usize).min(crate::sess_sort::KEYS.len() - 1)];
                self.sess_sort_rev = false;
                self.sel_session = 0;
                self.state_dirty = true;
            }
            Cmd::TabHistory => self.open_tab_history_view(),
            Cmd::Pause(0) => self.resume_listening("menu"),
            Cmd::Pause(s) => {
                self.pause_listening(Some(s as u64), None);
            }
            Cmd::Reopen(i) => {
                if let Err(e) = self.reopen_closed_at(i as usize) {
                    self.flash(format!("Could not reopen it: {e}"));
                }
            }
            Cmd::SessReverse => {
                self.sess_sort_rev = !self.sess_sort_rev;
                self.state_dirty = true;
            }
            Cmd::SessGroup(i) => {
                self.sess_group =
                    crate::sess_sort::GROUPS[(i as usize).min(crate::sess_sort::GROUPS.len() - 1)];
                self.sel_session = 0;
                self.state_dirty = true;
            }
            Cmd::Layout(i) => {
                let m = crate::layout::MODES[(i as usize).min(crate::layout::MODES.len() - 1)];
                self.set_layout(m);
            }
            Cmd::AssistantAccount(a) => self.set_assistant_account(a),
            Cmd::AssistantModel(m) => match self.switch_assistant(None, None, Some(&m)) {
                Ok(say) | Err(say) => self.flash(say),
            },
            Cmd::AssistantProvider(p) => match self.switch_assistant(Some(&p), None, None) {
                Ok(say) | Err(say) => self.flash(say),
            },
            Cmd::OpenSetup => self.open_setup(),
            Cmd::AssistantRemote => {
                // A click is the user's own request; turning it on still
                // asks once (the panel's question), off is immediate.
                let r = if self.assistant.remote.is_some() {
                    self.disable_remote()
                } else if self.remote_click_armed() {
                    let name = self.remote_default_name();
                    self.enable_remote(&name)
                } else {
                    Ok(format!(
                        "Remote Control lets anyone signed in to this claude.ai account talk to the assistant, with its admin tools. Pick Remote Control again within 15 s to turn it on as '{}'.",
                        self.remote_default_name()
                    ))
                };
                match r {
                    Ok(s) | Err(s) => self.flash(s),
                }
            }
            Cmd::AssistantLogin => match self.assistant_grok_login() {
                Ok(say) | Err(say) => self.flash(say),
            },
            Cmd::OpenAccount(a) => {
                let list: Vec<usize> = match a {
                    Some(a) => vec![a],
                    None => (0..self.cfg.accounts.len())
                        .filter(|a| self.account_closed(*a))
                        .collect(),
                };
                let say = self.open_accounts(&list, false);
                self.flash(say);
            }
            Cmd::AssistantRules => self.open_learned_view(),
            Cmd::AssistantPrompt => self.open_prompt_view(),
            Cmd::AssistantAdmin => {
                self.assistant.show_admin = !self.assistant.show_admin;
                self.assistant.history = None;
            }
            Cmd::AssistantDetails => self.assistant.show_details = !self.assistant.show_details,
            Cmd::PanelMode(i) => {
                let m = crate::app_talkback::PANEL_MODES
                    .get(i as usize)
                    .copied()
                    .unwrap_or("docked");
                let _ = self.set_panel_mode(m);
            }
            Cmd::SpeakTyped => {
                let on = !self.cfg.assistant.speak_typed;
                self.set_speak_typed(on);
                self.flash(if on {
                    "Replies to typed messages are spoken too"
                } else {
                    "Typed messages get a text answer only"
                });
            }
            Cmd::AssistantSettings => {
                self.view = crate::app::View::Settings;
                self.settings_section = crate::settings::SECTIONS
                    .iter()
                    .position(|(s, _)| *s == crate::settings::Section::Assistant)
                    .unwrap_or(0);
                self.settings_sel = 0;
            }
            Cmd::ShowPath => {
                self.cfg.show_path = !self.cfg.show_path;
                let _ = crate::settings::write(
                    &crate::config::Config::path(),
                    &crate::settings::Key::Global("show_path"),
                    Some(toml_edit::value(self.cfg.show_path)),
                );
                self.flash(if self.cfg.show_path {
                    "Folders shown in pane headers"
                } else {
                    "Folders hidden"
                });
            }
            Cmd::Permission(i) => {
                let p = self.focus;
                if let Some(a) = self.panes[p].account {
                    let m =
                        crate::settings::MODES[(i as usize).min(crate::settings::MODES.len() - 1)];
                    self.set_permission_mode(p, a, m, false);
                }
            }
            Cmd::TrainWake => self.start_wake_training(),
            Cmd::Tour => self.on_command(KeyEvent::new(
                KeyCode::Char('T'),
                crossterm::event::KeyModifiers::NONE,
            )),
        }
    }

    /// Keys while a menu is open. Always consumed.
    pub fn on_menu_key(&mut self, k: KeyEvent) {
        let Some(mut o) = self.menu_state().cloned() else {
            return;
        };
        let in_sub = o.sub.is_some();
        let (id, sel) = match o.sub {
            Some((s, sel)) => (s, sel),
            None => (o.id, o.sel),
        };
        let es = entries(self, id);
        let step = |from: Option<usize>, d: isize| -> Option<usize> {
            let n = es.len() as isize;
            let mut i = from
                .map(|x| x as isize)
                .unwrap_or(if d > 0 { -1 } else { n });
            for _ in 0..n {
                i = (i + d).rem_euclid(n);
                if usable(&es[i as usize]) || es[i as usize].sub.is_some() && !es[i as usize].sep {
                    return Some(i as usize);
                }
            }
            from
        };
        let set = |o: &mut Open, v: Option<usize>| match &mut o.sub {
            Some((_, s)) => *s = v,
            None => o.sel = v,
        };
        match k.code {
            KeyCode::Esc if in_sub => o.sub = None,
            KeyCode::Esc => {
                self.modal = Modal::None;
                return;
            }
            KeyCode::Up => set(&mut o, step(sel, -1)),
            KeyCode::Down => set(&mut o, step(sel, 1)),
            KeyCode::Left if in_sub => o.sub = None,
            KeyCode::Left | KeyCode::Right
                if !in_sub
                    && !(k.code == KeyCode::Right
                        && sel.and_then(|i| es.get(i)).is_some_and(|e| e.sub.is_some())) =>
            {
                // Next or previous menu along the bar.
                let i = MenuId::TOP.iter().position(|m| *m == o.id).unwrap_or(0) as isize;
                let d = if k.code == KeyCode::Left { -1 } else { 1 };
                let next = MenuId::TOP[(i + d).rem_euclid(MenuId::TOP.len() as isize) as usize];
                self.open_menu(next);
                return;
            }
            KeyCode::Right | KeyCode::Enter => {
                if let Some(e) = sel.and_then(|i| es.get(i)).cloned() {
                    if let (Some(s), false) = (e.sub, in_sub) {
                        if e.disabled.is_none() {
                            o.sub = Some((s, None));
                            let first = entries(self, s).iter().position(usable);
                            o.sub = Some((s, first));
                        }
                    } else if usable(&e) {
                        self.menu_run(&e);
                        return;
                    }
                } else if k.code == KeyCode::Enter {
                    self.modal = Modal::None;
                    return;
                }
            }
            KeyCode::Char(c) => {
                // Type to select: the next row starting with that letter.
                let c = c.to_ascii_lowercase();
                let start = sel.map(|s| s + 1).unwrap_or(0);
                let n = es.len();
                if let Some(j) = (0..n)
                    .map(|d| (start + d) % n)
                    .find(|&j| !es[j].sep && es[j].label.to_lowercase().starts_with(c))
                {
                    set(&mut o, Some(j));
                }
            }
            _ => {}
        }
        self.modal = Modal::Menu(o);
    }

    /// Mouse over the menu bar or an open menu: hovering another title
    /// switches menus; hovering a row selects it and opens its submenu.
    pub fn on_menu_hover(&mut self, col: u16, row: u16) {
        let Some(o) = self.menu_state().cloned() else {
            return;
        };
        // Another menu title (it sits under the popup's regions).
        let title = self
            .last_hits
            .regions
            .iter()
            .find(|r| {
                crate::hits::contains(r.rect, col, row) && matches!(r.action, UiAction::MenuOpen(_))
            })
            .map(|r| r.action.clone());
        if let Some(UiAction::MenuOpen(id)) = title {
            if id != o.id {
                self.open_menu(id);
            }
            return;
        }
        match self.region_at(col, row).map(|r| r.action) {
            Some(UiAction::MenuRow(id, i)) => {
                let mut o = o;
                if id == o.id {
                    o.sel = Some(i);
                    let e = entries(self, id).get(i).cloned();
                    o.sub = e
                        .and_then(|e| e.sub.filter(|_| e.disabled.is_none()))
                        .map(|s| (s, None));
                } else if let Some((s, _)) = o.sub {
                    if s == id {
                        o.sub = Some((s, Some(i)));
                    }
                }
                self.modal = Modal::Menu(o);
            }
            _ => {}
        }
    }

    /// A click on a menu row.
    pub fn menu_click(&mut self, id: MenuId, i: usize) {
        let es = entries(self, id);
        let Some(e) = es.get(i).cloned() else { return };
        if let Some(s) = e.sub {
            if e.disabled.is_none() {
                if let Some(mut o) = self.menu_state().cloned() {
                    o.sel = Some(i);
                    o.sub = Some((s, None));
                    self.modal = Modal::Menu(o);
                }
            }
            return;
        }
        if usable(&e) {
            self.menu_run(&e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_stays_on_screen() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(place(area, (10, 0), 30, 10), Rect::new(10, 1, 30, 10));
        // Shifted left at the right edge.
        assert_eq!(place(area, (70, 0), 30, 10).x, 50);
        // Flipped above near the bottom.
        let r = place(area, (5, 22), 20, 8);
        assert!(r.y + r.height <= 22);
    }
}
