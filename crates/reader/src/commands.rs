//! The command registry, after pdfcraft's `engine/src/commands.rs`: every
//! user-facing action by id. Keys, the palette, menus and toolbar buttons
//! all run commands through `ReaderApp::run`; this table says what each is
//! called, which keys run it by default (in each keymap preset) and when it
//! can run.
//!
//! Escape (its cancel chain), Copy (egui's copy event), the mouse's back and
//! forward buttons and the view's own wheel and drag stay fixed.

use crate::keys::KeymapPreset;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Command {
    Open,
    CloseTab,
    NextTab,
    PreviousTab,
    Palette,
    Settings,
    ToggleSidebar,
    ToggleStats,
    ThemeSystem,
    ThemeLight,
    ThemeDark,
    KeysStandard,
    KeysVim,
    ScrollDown,
    ScrollUp,
    ScrollLeft,
    ScrollRight,
    NextPage,
    PreviousPage,
    NextHalfPage,
    PreviousHalfPage,
    FirstPage,
    LastPage,
    Back,
    Forward,
    ZoomIn,
    ZoomOut,
    FitWidth,
    RotateClockwise,
    RotateCounterclockwise,
    ToggleSpread,
    Reload,
    Present,
    Zen,
    PageOverview,
    SplitRight,
    SplitDown,
    FocusNextPane,
    ClosePane,
    LargerText,
    SmallerText,
    Find,
    FindNext,
    FindPrevious,
    SelectPage,
    RegionSelect,
    Bookmark,
    TranslateSelection,
    TranslateDocument,
    ShowTranslated,
    Assistant,
    QuickAsk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    App,
    Navigation,
    View,
    Text,
    Translation,
    Assistant,
}

impl Category {
    pub const ALL: [Self; 6] = [
        Self::App,
        Self::Navigation,
        Self::View,
        Self::Text,
        Self::Translation,
        Self::Assistant,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Navigation => "Navigation",
            Self::View => "View",
            Self::Text => "Text",
            Self::Translation => "Translation",
            Self::Assistant => "Assistant",
        }
    }
}

/// When a command can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Needs {
    Nothing,
    /// A document tab is active.
    Document,
    /// The active document has a text selection.
    Selection,
    /// The active document is reflowed (EPUB).
    Reflow,
    /// The active document has a translation.
    Translation,
    /// The stage is split into panes.
    Panes,
    /// The active document has fixed pages (not a reflowed book).
    FixedLayout,
}

impl Needs {
    /// Why a command needing this cannot run.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Nothing | Self::Document => "Open a document first",
            Self::Selection => "Select text first",
            Self::Reflow => "Only for reflowed books",
            Self::Translation => "Translate the document first",
            Self::Panes => "Split the view first",
            Self::FixedLayout => "Reflowed books don't turn or spread",
        }
    }
}

impl Command {
    pub const ALL: [Self; 52] = [
        Self::Open,
        Self::CloseTab,
        Self::NextTab,
        Self::PreviousTab,
        Self::Palette,
        Self::Settings,
        Self::ToggleSidebar,
        Self::ToggleStats,
        Self::ThemeSystem,
        Self::ThemeLight,
        Self::ThemeDark,
        Self::KeysStandard,
        Self::KeysVim,
        Self::ScrollDown,
        Self::ScrollUp,
        Self::ScrollLeft,
        Self::ScrollRight,
        Self::NextPage,
        Self::PreviousPage,
        Self::NextHalfPage,
        Self::PreviousHalfPage,
        Self::FirstPage,
        Self::LastPage,
        Self::Back,
        Self::Forward,
        Self::ZoomIn,
        Self::ZoomOut,
        Self::FitWidth,
        Self::RotateClockwise,
        Self::RotateCounterclockwise,
        Self::ToggleSpread,
        Self::Reload,
        Self::Present,
        Self::Zen,
        Self::PageOverview,
        Self::SplitRight,
        Self::SplitDown,
        Self::FocusNextPane,
        Self::ClosePane,
        Self::LargerText,
        Self::SmallerText,
        Self::Find,
        Self::FindNext,
        Self::FindPrevious,
        Self::SelectPage,
        Self::RegionSelect,
        Self::Bookmark,
        Self::TranslateSelection,
        Self::TranslateDocument,
        Self::ShowTranslated,
        Self::Assistant,
        Self::QuickAsk,
    ];

    /// The stable id kept in preferences.
    pub fn id(self) -> &'static str {
        match self {
            Self::Open => "app.open",
            Self::CloseTab => "app.close_tab",
            Self::NextTab => "app.next_tab",
            Self::PreviousTab => "app.previous_tab",
            Self::Palette => "app.command_palette",
            Self::Settings => "app.settings",
            Self::ToggleSidebar => "app.sidebar",
            Self::ToggleStats => "app.statistics",
            Self::ThemeSystem => "app.theme_system",
            Self::ThemeLight => "app.theme_light",
            Self::ThemeDark => "app.theme_dark",
            Self::KeysStandard => "app.keys_standard",
            Self::KeysVim => "app.keys_vim",
            Self::ScrollDown => "navigation.scroll_down",
            Self::ScrollUp => "navigation.scroll_up",
            Self::ScrollLeft => "navigation.scroll_left",
            Self::ScrollRight => "navigation.scroll_right",
            Self::NextPage => "navigation.next_page",
            Self::PreviousPage => "navigation.previous_page",
            Self::NextHalfPage => "navigation.next_half_page",
            Self::PreviousHalfPage => "navigation.previous_half_page",
            Self::FirstPage => "navigation.first_page",
            Self::LastPage => "navigation.last_page",
            Self::Back => "navigation.back",
            Self::Forward => "navigation.forward",
            Self::ZoomIn => "view.zoom_in",
            Self::ZoomOut => "view.zoom_out",
            Self::FitWidth => "view.fit_width",
            Self::RotateClockwise => "view.rotate_clockwise",
            Self::RotateCounterclockwise => "view.rotate_counterclockwise",
            Self::ToggleSpread => "view.two_page_spread",
            Self::Reload => "view.reload",
            Self::Present => "view.present",
            Self::Zen => "view.zen",
            Self::PageOverview => "view.page_overview",
            Self::SplitRight => "view.split_right",
            Self::SplitDown => "view.split_down",
            Self::FocusNextPane => "view.focus_next_pane",
            Self::ClosePane => "view.close_pane",
            Self::LargerText => "view.larger_text",
            Self::SmallerText => "view.smaller_text",
            Self::Find => "text.find",
            Self::FindNext => "text.find_next",
            Self::FindPrevious => "text.find_previous",
            Self::SelectPage => "text.select_page",
            Self::RegionSelect => "text.select_region",
            Self::Bookmark => "text.bookmark",
            Self::TranslateSelection => "translation.selection",
            Self::TranslateDocument => "translation.document",
            Self::ShowTranslated => "translation.show_translated",
            Self::Assistant => "assistant.toggle",
            Self::QuickAsk => "assistant.quick_ask",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open documents…",
            Self::CloseTab => "Close tab",
            Self::NextTab => "Next tab",
            Self::PreviousTab => "Previous tab",
            Self::Palette => "Command palette",
            Self::Settings => "Settings…",
            Self::ToggleSidebar => "Show or hide the sidebar",
            Self::ToggleStats => "Show or hide statistics",
            Self::ThemeSystem => "Theme: System",
            Self::ThemeLight => "Theme: Light",
            Self::ThemeDark => "Theme: Dark",
            Self::KeysStandard => "Keys: Standard",
            Self::KeysVim => "Keys: Vim",
            Self::ScrollDown => "Scroll down",
            Self::ScrollUp => "Scroll up",
            Self::ScrollLeft => "Scroll left",
            Self::ScrollRight => "Scroll right",
            Self::NextPage => "Next page",
            Self::PreviousPage => "Previous page",
            Self::NextHalfPage => "Down half a screen",
            Self::PreviousHalfPage => "Up half a screen",
            Self::FirstPage => "First page",
            Self::LastPage => "Last page",
            Self::Back => "Back",
            Self::Forward => "Forward",
            Self::ZoomIn => "Zoom in",
            Self::ZoomOut => "Zoom out",
            Self::FitWidth => "Fit width",
            Self::RotateClockwise => "Rotate clockwise",
            Self::RotateCounterclockwise => "Rotate counterclockwise",
            Self::ToggleSpread => "Two-page spread",
            Self::Reload => "Reload the document",
            Self::Present => "Present",
            Self::Zen => "Zen mode: hide everything but the pages",
            Self::PageOverview => "Page overview",
            Self::SplitRight => "Split right",
            Self::SplitDown => "Split down",
            Self::FocusNextPane => "Focus the next pane",
            Self::ClosePane => "Close the pane",
            Self::LargerText => "Larger text",
            Self::SmallerText => "Smaller text",
            Self::Find => "Find",
            Self::FindNext => "Find next",
            Self::FindPrevious => "Find previous",
            Self::SelectPage => "Select the page's text",
            Self::RegionSelect => "Select a region of the page",
            Self::Bookmark => "Bookmark this page",
            Self::TranslateSelection => "Translate the selection",
            Self::TranslateDocument => "Translate the document",
            Self::ShowTranslated => "Show translated or original pages",
            Self::Assistant => "Show or hide the assistant",
            Self::QuickAsk => "Quick ask about this page",
        }
    }

    /// Its Lucide icon, shown in the palette, menus and the dock.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Open => "folder-open",
            Self::CloseTab => "x",
            Self::NextTab => "chevron-right",
            Self::PreviousTab => "chevron-left",
            Self::Palette => "command",
            Self::Settings => "settings",
            Self::ToggleSidebar => "panel-left",
            Self::ToggleStats => "gauge",
            Self::ThemeSystem => "monitor",
            Self::ThemeLight => "sun",
            Self::ThemeDark => "moon",
            Self::KeysStandard | Self::KeysVim => "keyboard",
            Self::ScrollDown => "arrow-down",
            Self::ScrollUp => "arrow-up",
            Self::ScrollLeft => "arrow-left",
            Self::ScrollRight => "arrow-right",
            Self::NextPage => "chevron-down",
            Self::PreviousPage => "chevron-up",
            Self::NextHalfPage => "chevron-down",
            Self::PreviousHalfPage => "chevron-up",
            Self::FirstPage => "chevron-up",
            Self::LastPage => "chevron-down",
            Self::Back => "arrow-left",
            Self::Forward => "arrow-right",
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
            Self::FitWidth => "scan",
            Self::RotateClockwise => "rotate-cw",
            Self::RotateCounterclockwise => "rotate-ccw",
            Self::ToggleSpread => "book-open",
            Self::Reload => "refresh-cw",
            Self::Present => "presentation",
            Self::Zen => "eye-off",
            Self::PageOverview => "layout-grid",
            Self::SplitRight => "columns-2",
            Self::SplitDown => "rows-2",
            Self::FocusNextPane => "panels-top-left",
            Self::ClosePane => "x",
            Self::LargerText => "a-arrow-up",
            Self::SmallerText => "a-arrow-down",
            Self::Find | Self::FindNext | Self::FindPrevious => "search",
            Self::SelectPage => "type",
            Self::RegionSelect => "scan",
            Self::Bookmark => "bookmark-plus",
            Self::TranslateSelection | Self::TranslateDocument => "languages",
            Self::ShowTranslated => "globe",
            Self::Assistant => "sparkles",
            Self::QuickAsk => "message-square",
        }
    }

    pub fn category(self) -> Category {
        match self {
            Self::Open
            | Self::CloseTab
            | Self::NextTab
            | Self::PreviousTab
            | Self::Palette
            | Self::Settings
            | Self::ToggleSidebar
            | Self::ToggleStats
            | Self::ThemeSystem
            | Self::ThemeLight
            | Self::ThemeDark
            | Self::KeysStandard
            | Self::KeysVim => Category::App,
            Self::ScrollDown
            | Self::ScrollUp
            | Self::ScrollLeft
            | Self::ScrollRight
            | Self::NextPage
            | Self::PreviousPage
            | Self::NextHalfPage
            | Self::PreviousHalfPage
            | Self::FirstPage
            | Self::LastPage
            | Self::Back
            | Self::Forward => Category::Navigation,
            Self::ZoomIn
            | Self::ZoomOut
            | Self::FitWidth
            | Self::RotateClockwise
            | Self::RotateCounterclockwise
            | Self::ToggleSpread
            | Self::Reload
            | Self::Present
            | Self::Zen
            | Self::PageOverview
            | Self::SplitRight
            | Self::SplitDown
            | Self::FocusNextPane
            | Self::ClosePane
            | Self::LargerText
            | Self::SmallerText => Category::View,
            Self::Find
            | Self::FindNext
            | Self::FindPrevious
            | Self::SelectPage
            | Self::RegionSelect
            | Self::Bookmark => Category::Text,
            Self::TranslateSelection | Self::TranslateDocument | Self::ShowTranslated => {
                Category::Translation
            }
            Self::Assistant | Self::QuickAsk => Category::Assistant,
        }
    }

    /// The keys that run it in `preset` unless the user changed them, in
    /// `keys` notation; keys of a sequence are separated by spaces.
    pub fn default_keys(self, preset: KeymapPreset) -> &'static [&'static str] {
        match preset {
            KeymapPreset::Standard => self.standard_keys(),
            KeymapPreset::Vim => self.vim_keys(),
        }
    }

    fn standard_keys(self) -> &'static [&'static str] {
        match self {
            Self::Open => &["Ctrl+O"],
            Self::CloseTab => &["Ctrl+W"],
            Self::NextTab => &["Ctrl+Tab"],
            Self::PreviousTab => &["Ctrl+Shift+Tab"],
            Self::Palette => &["Ctrl+K", "Ctrl+Shift+P"],
            Self::Settings => &["Ctrl+Comma"],
            Self::ToggleSidebar => &["F9"],
            Self::ToggleStats => &["F12"],
            Self::ScrollDown => &["Down"],
            Self::ScrollUp => &["Up"],
            Self::ScrollLeft => &["Left"],
            Self::ScrollRight => &["Right"],
            Self::NextPage => &["PageDown", "Space"],
            Self::PreviousPage => &["PageUp"],
            Self::NextHalfPage => &["Shift+PageDown"],
            Self::PreviousHalfPage => &["Shift+PageUp"],
            Self::FirstPage => &["Home"],
            Self::LastPage => &["End"],
            Self::Back => &["Alt+Left"],
            Self::Forward => &["Alt+Right"],
            Self::ZoomIn => &["Plus", "Equals"],
            Self::ZoomOut => &["Minus"],
            Self::FitWidth => &["0"],
            Self::RotateClockwise => &["R"],
            Self::RotateCounterclockwise => &["Shift+R"],
            Self::Reload => &["F5", "Ctrl+R"],
            Self::Present => &["P"],
            Self::Zen => &["Z"],
            Self::PageOverview => &["Ctrl+G"],
            Self::SplitRight => &["Ctrl+Backslash"],
            Self::FocusNextPane => &["F6"],
            Self::Find => &["Ctrl+F"],
            Self::FindNext => &["F3"],
            Self::FindPrevious => &["Shift+F3"],
            Self::SelectPage => &["Ctrl+A"],
            Self::RegionSelect => &["S"],
            Self::Bookmark => &["Ctrl+D"],
            Self::TranslateSelection => &["Ctrl+T"],
            Self::Assistant => &["Ctrl+L"],
            Self::QuickAsk => &["Tab"],
            Self::ThemeSystem
            | Self::ThemeLight
            | Self::ThemeDark
            | Self::KeysStandard
            | Self::KeysVim
            | Self::LargerText
            | Self::SmallerText
            | Self::TranslateDocument
            | Self::ShowTranslated
            | Self::SplitDown
            | Self::ClosePane
            | Self::ToggleSpread => &[],
        }
    }

    /// After the GPUI Reader's vim preset, zathura and sioyek: Ctrl+W, F, D
    /// and O go to vim's meanings, so closing a tab, Find, Bookmark and Open
    /// move to Q, /, M and O.
    fn vim_keys(self) -> &'static [&'static str] {
        match self {
            Self::Open => &["O"],
            Self::CloseTab => &["Q"],
            Self::NextTab => &["G T", "Ctrl+Tab"],
            Self::PreviousTab => &["G Shift+T", "Ctrl+Shift+Tab"],
            Self::Palette => &["Colon", "Ctrl+K", "Ctrl+Shift+P"],
            Self::ScrollDown => &["J", "Down"],
            Self::ScrollUp => &["K", "Up"],
            Self::ScrollLeft => &["H", "Left"],
            Self::ScrollRight => &["L", "Right"],
            Self::NextPage => &["Shift+J", "Ctrl+F", "Space", "PageDown"],
            Self::PreviousPage => &["Shift+K", "Ctrl+B", "Shift+Space", "PageUp"],
            Self::NextHalfPage => &["Ctrl+D", "Shift+PageDown"],
            Self::PreviousHalfPage => &["Ctrl+U", "Shift+PageUp"],
            Self::FirstPage => &["G G", "Home"],
            Self::LastPage => &["Shift+G", "End"],
            Self::Back => &["Ctrl+O", "Alt+Left"],
            Self::Forward => &["Ctrl+I", "Alt+Right"],
            Self::ToggleSpread => &["D"],
            Self::SplitRight => &["Ctrl+W V", "Ctrl+Backslash"],
            Self::SplitDown => &["Ctrl+W S"],
            Self::FocusNextPane => &["Ctrl+W W", "Ctrl+W Ctrl+W", "F6"],
            Self::ClosePane => &["Ctrl+W Q", "Ctrl+W C"],
            Self::Find => &["Slash"],
            Self::FindNext => &["N", "F3"],
            Self::FindPrevious => &["Shift+N", "Shift+F3"],
            Self::Bookmark => &["M"],
            _ => self.standard_keys(),
        }
    }

    /// Whether a count typed before its keys (vim's `5j`) repeats it.
    /// First and last page take theirs as the page to go to.
    pub fn counted(self) -> bool {
        matches!(
            self,
            Self::ScrollDown
                | Self::ScrollUp
                | Self::ScrollLeft
                | Self::ScrollRight
                | Self::NextPage
                | Self::PreviousPage
                | Self::NextHalfPage
                | Self::PreviousHalfPage
                | Self::ZoomIn
                | Self::ZoomOut
                | Self::FindNext
                | Self::FindPrevious
        )
    }

    pub fn needs(self) -> Needs {
        match self {
            Self::Open
            | Self::NextTab
            | Self::PreviousTab
            | Self::Palette
            | Self::Settings
            | Self::ToggleSidebar
            | Self::ToggleStats
            | Self::ThemeSystem
            | Self::ThemeLight
            | Self::ThemeDark
            | Self::KeysStandard
            | Self::KeysVim => Needs::Nothing,
            Self::LargerText | Self::SmallerText => Needs::Reflow,
            Self::TranslateSelection => Needs::Selection,
            Self::ShowTranslated => Needs::Translation,
            Self::FocusNextPane | Self::ClosePane => Needs::Panes,
            Self::RotateClockwise
            | Self::RotateCounterclockwise
            | Self::ToggleSpread
            | Self::RegionSelect => Needs::FixedLayout,
            _ => Needs::Document,
        }
    }

    /// Whether its shortcut may fire while a text field has focus, when the
    /// shortcut has Ctrl or Alt or is a function key (see
    /// `keys::Shortcut::works_in_text`): what typing can't produce.
    pub fn in_text(self) -> bool {
        !matches!(
            self,
            Self::TranslateSelection
                | Self::SelectPage
                | Self::RegionSelect
                | Self::QuickAsk
                | Self::ScrollDown
                | Self::ScrollUp
                | Self::ScrollLeft
                | Self::ScrollRight
                | Self::NextPage
                | Self::PreviousPage
                | Self::NextHalfPage
                | Self::PreviousHalfPage
                | Self::FirstPage
                | Self::LastPage
                | Self::ZoomIn
                | Self::ZoomOut
                | Self::FitWidth
                | Self::RotateClockwise
                | Self::RotateCounterclockwise
                | Self::Present
                | Self::Zen
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_listed_once_with_a_unique_id() {
        let mut ids: Vec<&str> = Command::ALL.iter().map(|command| command.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), Command::ALL.len());
        for command in Command::ALL {
            assert_eq!(Command::from_id(command.id()), Some(command));
        }
    }
}
