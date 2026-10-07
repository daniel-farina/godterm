//! Clickable regions. Rendering registers every button, tab, row and area
//! it draws together with the action it stands for; mouse handling looks the
//! click up here instead of recomputing layout. Later regions sit on top.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::prompt::Choice;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    Overview,
    Approvals,
    Sessions,
    Picker,
    Palette,
    Dashboard,
    AccountMenu,
    SettingsSection,
    Settings,
    Loops,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerUi {
    NameField,
    CreateFolder,
    GitInit,
    AllAccounts,
    OpenExisting,
    EditBase,
    Create,
    RemoveRecent(usize),
}

/// Parts of the Add account dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddUi {
    /// Anywhere inside: keeps the dialog open.
    Inside,
    Field(u8),
    Agent(u8),
    Color(u8),
    Recent(u8),
    /// 0 previous, 1 next.
    Mode(u8),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrainUi {
    Start,
    Skip,
    Save,
    Retrain,
    Reset,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    /// Same as Ctrl-a followed by this key.
    Key(char),
    FocusPane(usize),
    /// An empty grid cell's "New tab on… ▾" (which cell).
    EmptySlotMenu(usize),
    /// Open a new pane on this account and pick a folder for its tab.
    NewPaneFor(usize),
    /// Learned rules view: a key, a row, a toggle; opening it.
    LearnedKey(char),
    LearnedRow(usize),
    LearnedToggle(usize),
    OpenLearned,
    PromptKey(char),
    PromptRow(usize),
    OpenPrompt,
    /// Tab history view: a key's action, a row.
    ThKey(char),
    ThRow(usize),
    /// End a listening pause.
    ResumeListening,
    /// Close a pane's window (asks first).
    CloseWindow(usize),
    /// Reopen the last closed tab or window (the Undo toast).
    ReopenClosed,
    /// A pane header's background: focus, and drag to swap panes.
    PaneHeader(usize),
    /// Move a pane: 0..3 left, right, up, down; 10 + n to slot n.
    MovePane(usize, u8),
    /// Click inside a pane's terminal area.
    PaneBody(usize),
    SelectTab(usize, usize),
    CloseTab(usize, usize),
    NewTab(usize),
    Zoom(usize),
    Restart(usize),
    AccountMenu(usize),
    /// Footer usage area: open the dashboard on this account.
    UsageOf(usize),
    /// Footer of a logged out slot: start the login.
    LoginPane(usize),
    /// Answer the prompt shown in (pane, tab).
    Answer(usize, usize, Choice),
    Jump(usize, usize),
    /// Row `i` of a list: click selects, double click activates.
    Row(List, usize),
    /// Voice mode button: off, push to talk, wake word.
    VoiceCycle,
    Mic,
    ModalOk,
    ModalCancel,
    TourNext,
    TourSkip,
    TourStart,
    /// Collapse or expand pane `i`'s tab list.
    SidebarToggle(usize),
    /// Scroll pane `i`'s tab list by this many tabs.
    TabsScroll(usize, isize),
    /// Set (or clear) pane `i`'s tab list position.
    TabPos(usize, Option<crate::slot::TabPos>),
    /// Set the permission mode of the account in pane `.0`.
    SetPerm(usize, &'static str),
    /// Flip auto trust for the account in pane `.0`.
    ToggleTrust(usize),
    /// New Tab dialog controls.
    Picker(PickerUi),
    /// Settings rows: step a value (+1 / -1), reset it, edit text.
    SettingStep(usize, i64),
    SettingReset(usize),
    SettingEdit(usize),
    OpenConfig,
    RunDoctor,
    /// Settings > Setup: install an item ("voice": the voice pack).
    SetupInstall(String),
    /// Settings > Setup: the whisper model to offer.
    SetupWhisper(String),
    /// Settings > Setup: do not open it at start.
    SetupDontShow,
    /// The assistant panel's Send button.
    AssistantSend,
    /// An action chip in the assistant panel: show or hide its raw call.
    AssistantChip(usize),
    /// Settings > About: check for a new version now.
    UpdateCheck,
    /// Settings > About: do not offer this version again.
    UpdateSkip,
    /// Settings > About: godterm install (true: with --dock).
    Install(bool),
    /// Play a sample of the selected talk back voice.
    VoicePreview,
    /// Open wake word training.
    TrainWake,
    /// Settings > Voice: "Train my voice" (speaker lock enrollment).
    TrainVoice,
    /// Settings > Voice: the macOS mic mode picker (Voice Isolation).
    MicModes,
    /// A draggable border between panes.
    Border,
    /// Back to the grid of every pane (the logo, Grid, Back, x).
    Home,
    /// Sessions view: source chip, an action key, a dialog choice.
    SessSource(usize),
    SessKey(char),
    SessTarget(usize),
    SessConflict(u8),
    SessConfirm,
    /// Move tab picker row (single click selects, double confirms), its
    /// Move / Copy and same folder toggles, the confirm button.
    MoveRow(usize),
    MoveCopy(bool),
    MoveFolder,
    MoveGo,
    /// Busy tab: wait (false) or interrupt (true).
    MoveBusy(bool),
    /// Tab menu entry.
    TabMenuItem(usize),
    /// A tab group's header in pane .0's list: click folds, right click
    /// opens its menu, a dragged tab dropped on it joins.
    GroupHeader(usize, usize),
    GroupMenuItem(usize),
    GroupPickItem(usize),
    GroupMoveTo(usize),
    /// A Settings header: fold or unfold it.
    SettingsGroup(&'static str),
    /// Settings: test the Grok voice credential, or remove the API key.
    GrokTest,
    GrokRemoveKey,
    /// Settings: record 3 s and transcribe it with Grok.
    SttTest,
    /// Settings: list the audio devices again (after a timeout).
    RetryDevices,
    /// Settings: open the Grok login picker.
    OpenGrokLogins,
    /// The Grok login picker: (card, action) (see grok_logins).
    GrokLoginAct(usize, u8),
    /// Tabs ▾ > Sort tabs (the focused pane).
    SortTabs(crate::tab_groups::TabSort),
    /// Undo the last tab move.
    UndoTabMove,
    /// Loops view: jump to the selected loop's tab, stop it, stop all.
    LoopJump,
    LoopStop,
    LoopStopAll,
    /// Memory: free now.
    FreeNow,
    /// Assistant: new conversation.
    AssistantNew,
    /// Assistant panel: the History tab, a row, read, resume, delete.
    AssistantHistory,
    HistRow(usize),
    HistResume,
    HistDelete,
    HistBack,
    /// A pane header's folder: click copies, double click opens Finder,
    /// right click opens its menu; the menu's rows.
    PathClick(usize),
    /// The edge of pane `.0`'s tab list: drag to resize, double click resets.
    SidebarEdge(usize),
    /// A color choice in the color picker (usize::MAX: inside, no-op).
    ColorPick(usize),
    /// Open the color picker.
    OpenColor(crate::color_pick::Target),
    /// The Add account dialog.
    AddAcct(AddUi),
    PathMenuItem(usize, u8),
    /// Sessions view: the harness filter (0 claude, 1 grok) and subagents.
    SessHarness(u8),
    SessSubagents,
    /// Sessions: show headless / temp folder sessions.
    SessHeadless,
    /// Sessions: a column header (index into sess_sort::KEYS).
    SessSort(u8),
    /// The Bring here dialog: 0 take over (wait), 1 interrupt, 2 copy.
    TakeOverDo(u8),
    /// The red mute button (menu bar), Ctrl-a X.
    MuteToggle,
    /// A menu bar title, a row of an open menu, a menu only command.
    MenuOpen(crate::menus::MenuId),
    MenuRow(crate::menus::MenuId, usize),
    Menu(crate::menus::Cmd),
    /// The Voice dropdown (menu bar button, status bar chip), its rows.
    VoiceMenu,
    VoiceMode(u8),
    /// The » menu of buttons that did not fit, its rows.
    MenuOverflow,
    OverflowItem(usize),
    /// The ✎ on a tab: rename it in place.
    RenameTab(usize, usize),
    /// Move dialog: focus the tab name field, save the name only.
    MoveName,
    MoveRenameOnly,
    /// Suggested move of pane .0's tab to account .1.
    SuggestMove(usize, usize),
    /// Account menu: split, hide or close pane `.0`.
    SplitPane(usize),
    HidePane(usize),
    ClosePane(usize),
    ShowHidden,
    /// Wake word training dialog buttons.
    Train(TrainUi),
    LoginAccount(usize),
    RemoveAccount(usize),
    /// Account menu entries.
    SwitchAccount(usize, usize),
    Login(usize),
    Logout(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub rect: Rect,
    pub action: UiAction,
    /// Shown in the status bar while hovered.
    pub hint: String,
}

#[derive(Debug, Default, Clone)]
pub struct Hits {
    pub regions: Vec<Region>,
}

impl Hits {
    pub fn clear(&mut self) {
        self.regions.clear();
    }

    pub fn add(&mut self, rect: Rect, action: UiAction, hint: impl Into<String>) {
        if rect.width > 0 && rect.height > 0 {
            self.regions.push(Region {
                rect,
                action,
                hint: hint.into(),
            });
        }
    }

    /// Topmost region containing (col, row).
    #[cfg(test)]
    pub fn at(&self, col: u16, row: u16) -> Option<&Region> {
        self.regions
            .iter()
            .rev()
            .find(|r| contains(r.rect, col, row))
    }

    /// First region with exactly this action (tests and the tour).
    pub fn find(&self, action: &UiAction) -> Option<&Region> {
        self.regions.iter().find(|r| &r.action == action)
    }
}

pub fn contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

/// Draw "[ label ]" style button text at (x, y), clipped to `limit` (the
/// right edge), register it, and return the next free column.
#[allow(clippy::too_many_arguments)]
pub fn button(
    buf: &mut Buffer,
    hits: &mut Hits,
    hover: Option<(u16, u16)>,
    x: u16,
    y: u16,
    limit: u16,
    label: &str,
    action: UiAction,
    hint: &str,
    accent: Color,
) -> u16 {
    let text = format!(" {label} ");
    let w = unicode_width::UnicodeWidthStr::width(text.as_str()) as u16;
    if x >= limit || y >= buf.area.y + buf.area.height {
        return x;
    }
    let w = w.min(limit - x);
    let rect = Rect::new(x, y, w, 1);
    let hovered = hover.is_some_and(|(c, r)| contains(rect, c, r));
    let style = if hovered {
        Style::default()
            .fg(Color::Rgb(28, 28, 28))
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::FG).bg(theme::SEL_BG)
    };
    for (i, ch) in text.chars().take(w as usize).enumerate() {
        if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
    hits.add(rect, action, hint);
    x + w + 1
}

/// Write plain text (not clickable) and return the next column.
pub fn text(buf: &mut Buffer, x: u16, y: u16, limit: u16, s: &str, style: Style) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= limit {
            break;
        }
        let w = unicode_width(ch);
        if cx + w > limit {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
        cx += w;
    }
    cx
}

fn unicode_width(ch: char) -> u16 {
    unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topmost_region_wins() {
        let mut h = Hits::default();
        h.add(Rect::new(0, 0, 10, 10), UiAction::FocusPane(0), "pane");
        h.add(Rect::new(2, 2, 3, 1), UiAction::Key('o'), "overview");
        assert_eq!(h.at(3, 2).unwrap().action, UiAction::Key('o'));
        assert_eq!(h.at(8, 8).unwrap().action, UiAction::FocusPane(0));
        assert!(h.at(20, 20).is_none());
        h.add(Rect::new(0, 0, 0, 5), UiAction::Mic, "empty");
        assert!(
            h.find(&UiAction::Mic).is_none(),
            "zero sized regions are dropped"
        );
    }

    #[test]
    fn buttons_register_and_clip() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
        let mut h = Hits::default();
        let next = button(
            &mut buf,
            &mut h,
            None,
            1,
            0,
            20,
            "Help",
            UiAction::Key('?'),
            "help",
            theme::SAND,
        );
        assert_eq!(next, 8);
        assert_eq!(h.regions[0].rect, Rect::new(1, 0, 6, 1));
        assert_eq!(buf[(2, 0)].symbol(), "H");
        // Clipped at the limit.
        let n2 = button(
            &mut buf,
            &mut h,
            Some((17, 0)),
            15,
            0,
            20,
            "Overview",
            UiAction::Key('o'),
            "",
            theme::SAND,
        );
        assert_eq!(n2, 21);
        assert_eq!(h.regions[1].rect.width, 5);
        assert_eq!(buf[(17, 0)].bg, theme::SAND, "hover highlight");
    }
}
