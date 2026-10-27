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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddUi {
    Inside,
    Field(u8),
    Agent(u8),
    Color(u8),
    Recent(u8),

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
    Key(char),
    FocusPane(usize),

    EmptySlotMenu(usize),

    NewPaneFor(usize),

    LearnedKey(char),
    LearnedRow(usize),
    LearnedToggle(usize),
    OpenLearned,
    PromptKey(char),
    PromptRow(usize),
    OpenPrompt,

    ThKey(char),
    ThRow(usize),

    ResumeListening,

    CloseWindow(usize),

    ReopenClosed,

    PaneHeader(usize),

    MovePane(usize, u8),

    PaneBody(usize),
    SelectTab(usize, usize),
    CloseTab(usize, usize),
    NewTab(usize),
    Zoom(usize),
    Restart(usize),
    AccountMenu(usize),

    UsageOf(usize),

    LoginPane(usize),

    Answer(usize, usize, Choice),
    Jump(usize, usize),

    Row(List, usize),

    VoiceCycle,
    Mic,
    ModalOk,
    ModalCancel,
    TourNext,
    TourSkip,
    TourStart,

    Onboarding(u8),

    SidebarToggle(usize),

    TabsScroll(usize, isize),

    TabPos(usize, Option<crate::slot::TabPos>),

    SetPerm(usize, &'static str),

    ToggleTrust(usize),

    Picker(PickerUi),

    SettingStep(usize, i64),
    SettingReset(usize),
    SettingEdit(usize),
    OpenConfig,
    RunDoctor,

    SetupInstall(String),

    SetupWhisper(String),

    SetupDontShow,

    BrowserRefresh,

    BrowserSkill,

    BrowserDefault,

    BrowserLoginLinks,

    TabBrowser(usize),

    BrowserRow(usize),

    BrowserClear,

    BrowserAlso,

    GitKey(char),

    GitRow(usize),

    GitOpen(usize, usize),

    GitCfgOpen,

    GitCfgKey(char),

    GitCfgRow(usize),

    BrowserOpenTab,

    AssistantFocus,

    ChatBar,

    SchedArea,

    AssistantRemoteLink,

    AssistantSend,

    AssistantPanel(crate::app_panelwin::PanelCtl),

    PaneWidth(usize, bool),

    AssistantChip(usize),

    UpdateCheck,

    UpdateSkip,

    Install(bool),

    VoicePreview,

    PickVoiceClip,

    TrainWake,

    TrainVoice,

    MicModes,

    Border,

    Home,

    SessSource(usize),
    SessKey(char),
    SessTarget(usize),
    SessConflict(u8),
    SessConfirm,

    MoveRow(usize),
    MoveCopy(bool),
    MoveFolder,
    MoveGo,

    MoveBusy(bool),

    TabMenuItem(usize),

    TabOpt(usize),
    TabOptColor(usize),
    TabOptGroup(usize),
    TabTitleField,
    TabPanelSec(bool),
    MoveSessionOnly,

    GroupHeader(usize, usize),

    ArchivedHeader(usize),

    ArchivedRow(usize, String),

    ArchMenuItem(usize),
    GroupMenuItem(usize),
    GroupPickItem(usize),
    GroupMoveTo(usize),

    SettingsGroup(&'static str),

    GrokTest,
    GrokRemoveKey,

    SttTest,

    RetryDevices,

    OpenGrokLogins,

    GrokLoginAct(usize, u8),

    SortTabs(crate::tab_groups::TabSort),

    UndoTabMove,

    LoopJump,
    LoopStop,

    LoopCard,
    LoopStopAll,

    FreeNow,

    AssistantNew,

    AssistantHistory,
    HistRow(usize),
    HistResume,
    HistDelete,
    HistBack,

    PathClick(usize),

    SidebarEdge(usize),

    FoldAccount(usize),

    FoldSideways(usize),

    TabSearch(usize),

    TabSearchClear(usize),

    TabSortMenu(usize),

    ColorPick(usize),

    OpenColor(crate::color_pick::Target),

    AddAcct(AddUi),
    PathMenuItem(usize, u8),

    SessHarness(u8),
    SessSubagents,

    SessHeadless,

    SessSort(u8),

    TakeOverDo(u8),

    MuteToggle,

    SpeakerToggle,

    AssistantConversation,

    AssistantAnswer(bool),

    OpenUpdates,
    UpdatesDownload,
    UpdatesRestart,
    UpdatesRestartGo,
    UpdatesLater,
    UpdatesPage,

    FailoverMove,
    FailoverDismiss,

    MenuOpen(crate::menus::MenuId),
    MenuRow(crate::menus::MenuId, usize),
    Menu(crate::menus::Cmd),

    FindAllRow(usize),

    VoiceMenu,

    MicMenu(usize),

    MicMoveTo(u64),
    MicToAssistant,
    VoiceMode(u8),

    MenuOverflow,
    OverflowItem(usize),

    RenameTab(usize, usize),

    MoveName,

    SuggestMove(usize, usize),

    SplitPane(usize),
    HidePane(usize),
    ClosePane(usize),
    ShowHidden,

    Train(TrainUi),
    LoginAccount(usize),
    RemoveAccount(usize),

    SwitchAccount(usize, usize),

    TabModel(usize, usize),

    LoopsDrawer(usize),
    LoopsDrawerEdge(usize),
    LoopsNew(usize),
    LoopRow(usize, usize),
    LoopAction(usize, usize, u8),

    SchedToggle,
    SchedRow(String),
    SchedAct(String, u8),

    SchedGroup(String),

    SchedCardAct(u8),
    SchedCardInside,
    Login(usize),
    Logout(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub rect: Rect,
    pub action: UiAction,

    pub hint: String,
}

#[derive(Debug, Default, Clone)]
pub struct Hits {
    pub regions: Vec<Region>,

    pub tab_lists: Vec<(usize, usize, usize)>,
}

impl Hits {
    pub fn clear(&mut self) {
        self.regions.clear();
        self.tab_lists.clear();
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

    #[cfg(test)]
    pub fn at(&self, col: u16, row: u16) -> Option<&Region> {
        self.regions
            .iter()
            .rev()
            .find(|r| contains(r.rect, col, row))
    }

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
