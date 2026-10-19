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
    /// Opens a document by path from the palette, like Zed's in-app path
    /// prompt.
    Open,
    /// Opens documents through the system's file dialog.
    OpenWithDialog,
    CloseTab,
    NextTab,
    PreviousTab,
    /// The book switcher held open (Ctrl+Tab): the books in recent order,
    /// letting go keeps the one shown (frame 10a).
    SwitchBook,
    /// [`Command::SwitchBook`] from the least recent book.
    SwitchBookBack,
    /// The book switcher opened until a book is picked.
    BookSwitcher,
    Palette,
    Settings,
    KeyboardShortcuts,
    /// A new tab showing the launcher (frame 04b).
    NewTab,
    /// The reading activity tab (frame 04c).
    ReadingActivity,
    /// The library tab (frame 04d).
    Library,
    /// The library's imports, AI pass and folder (frames 04g–04k, 06d).
    LibraryImport,
    LibraryAddByIdentifier,
    LibraryEnrichAll,
    LibraryReviewAi,
    LibraryImportZotero,
    LibraryMove,
    LibraryPauseJobs,
    /// The active document's note in the vault, in the system's editor.
    LibraryOpenNote,
    /// The sidebar's item card for the active document (frame 03b).
    SidebarItem,
    /// The home tab.
    Home,
    /// Gamepad mode (frames 09c–j), on or off.
    GamepadMode,
    /// Gamepad mode's menu (frame 09i).
    GamepadMenu,
    ToggleSidebar,
    ToggleStats,
    ThemeSystem,
    ThemeLight,
    ThemeDark,
    ToggleTransparency,
    /// The chrome a point larger, smaller, or back to the design's size
    /// (Settings › Appearance › ui scale).
    UiScaleUp,
    UiScaleDown,
    UiScaleReset,
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
    /// The whole page in the view.
    FitPage,
    /// A page chosen on a sheet of pages (frame 09h).
    GoToPage,
    RotateClockwise,
    RotateCounterclockwise,
    ToggleSpread,
    Reload,
    Present,
    /// The window fullscreen or not.
    ToggleFullscreen,
    Zen,
    PageOverview,
    /// The sidebar on the document's outline, or not.
    Outline,
    /// One or two columns of thumbnails in the Pages sidebar.
    PageColumns,
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
    /// Copies the MCP server's config for a coding agent started by hand.
    McpConfig,
    /// Presenting's tools (frames 02h, 02i): their keys work only while
    /// presenting, where they shadow the others.
    PresentDraw,
    PresentHighlight,
    PresentErase,
    /// Clears the page's ink.
    PresentClear,
    /// Undoes the page's latest drawing, erasing or clearing.
    PresentUndo,
    PresentRedo,
    /// Shows the pointer as the laser, or hides it.
    PresentCursor,
    PresentBlack,
    PresentWhite,
    /// Starts the clock, then pauses and resumes it.
    PresentTimer,
    PresentTimerStop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    App,
    Navigation,
    View,
    Text,
    Translation,
    Assistant,
    Present,
}

impl Category {
    pub const ALL: [Self; 7] = [
        Self::App,
        Self::Navigation,
        Self::View,
        Self::Present,
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
            Self::Present => "Presenting",
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
    /// The active document is being presented.
    Presenting,
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
            Self::Presenting => "Only while presenting",
        }
    }
}

impl Command {
    pub const ALL: [Self; 93] = [
        Self::Open,
        Self::OpenWithDialog,
        Self::CloseTab,
        Self::NextTab,
        Self::PreviousTab,
        Self::SwitchBook,
        Self::SwitchBookBack,
        Self::BookSwitcher,
        Self::Palette,
        Self::Settings,
        Self::KeyboardShortcuts,
        Self::NewTab,
        Self::ReadingActivity,
        Self::Library,
        Self::LibraryImport,
        Self::LibraryAddByIdentifier,
        Self::LibraryEnrichAll,
        Self::LibraryReviewAi,
        Self::LibraryImportZotero,
        Self::LibraryMove,
        Self::LibraryPauseJobs,
        Self::LibraryOpenNote,
        Self::SidebarItem,
        Self::Home,
        Self::GamepadMode,
        Self::GamepadMenu,
        Self::ToggleSidebar,
        Self::ToggleStats,
        Self::ThemeSystem,
        Self::ThemeLight,
        Self::ThemeDark,
        Self::ToggleTransparency,
        Self::UiScaleUp,
        Self::UiScaleDown,
        Self::UiScaleReset,
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
        Self::FitPage,
        Self::GoToPage,
        Self::RotateClockwise,
        Self::RotateCounterclockwise,
        Self::ToggleSpread,
        Self::Reload,
        Self::Present,
        Self::ToggleFullscreen,
        Self::Zen,
        Self::PageOverview,
        Self::Outline,
        Self::PageColumns,
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
        Self::McpConfig,
        Self::PresentDraw,
        Self::PresentHighlight,
        Self::PresentErase,
        Self::PresentClear,
        Self::PresentUndo,
        Self::PresentRedo,
        Self::PresentCursor,
        Self::PresentBlack,
        Self::PresentWhite,
        Self::PresentTimer,
        Self::PresentTimerStop,
    ];

    /// The stable id kept in preferences.
    pub fn id(self) -> &'static str {
        match self {
            Self::Open => "app.open",
            Self::OpenWithDialog => "app.open_with_dialog",
            Self::CloseTab => "app.close_tab",
            Self::NextTab => "app.next_tab",
            Self::PreviousTab => "app.previous_tab",
            Self::SwitchBook => "app.switch_book",
            Self::SwitchBookBack => "app.switch_book_back",
            Self::BookSwitcher => "app.book_switcher",
            Self::Palette => "app.command_palette",
            Self::Settings => "app.settings",
            Self::KeyboardShortcuts => "app.keyboard_shortcuts",
            Self::NewTab => "app.new_tab",
            Self::ReadingActivity => "app.reading_activity",
            Self::Library => "app.library",
            Self::LibraryImport => "library.import",
            Self::LibraryAddByIdentifier => "library.add_by_identifier",
            Self::LibraryEnrichAll => "library.enrich_all",
            Self::LibraryReviewAi => "library.review_ai",
            Self::LibraryImportZotero => "library.import_zotero",
            Self::LibraryMove => "library.move",
            Self::LibraryPauseJobs => "library.pause_jobs",
            Self::LibraryOpenNote => "library.open_note",
            Self::SidebarItem => "view.sidebar_item",
            Self::Home => "app.home",
            Self::GamepadMode => "app.gamepad_mode",
            Self::GamepadMenu => "app.gamepad_menu",
            Self::ToggleTransparency => "app.toggle_transparency",
            Self::UiScaleUp => "app.ui_scale_up",
            Self::UiScaleDown => "app.ui_scale_down",
            Self::UiScaleReset => "app.ui_scale_reset",
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
            Self::FitPage => "view.fit_page",
            Self::GoToPage => "view.go_to_page",
            Self::RotateClockwise => "view.rotate_clockwise",
            Self::RotateCounterclockwise => "view.rotate_counterclockwise",
            Self::ToggleSpread => "view.two_page_spread",
            Self::Reload => "view.reload",
            Self::Present => "view.present",
            Self::ToggleFullscreen => "view.fullscreen",
            Self::Zen => "view.zen",
            Self::PageOverview => "view.page_overview",
            Self::Outline => "view.outline",
            Self::PageColumns => "view.page_columns",
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
            Self::McpConfig => "assistant.mcp_config",
            Self::PresentDraw => "present.draw",
            Self::PresentHighlight => "present.highlight",
            Self::PresentErase => "present.erase",
            Self::PresentClear => "present.clear",
            Self::PresentUndo => "present.undo",
            Self::PresentRedo => "present.redo",
            Self::PresentCursor => "present.cursor",
            Self::PresentBlack => "present.black",
            Self::PresentWhite => "present.white",
            Self::PresentTimer => "present.timer",
            Self::PresentTimerStop => "present.timer_stop",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open document…",
            Self::OpenWithDialog => "Open with system dialog…",
            Self::CloseTab => "Close tab",
            Self::NextTab => "Next tab",
            Self::PreviousTab => "Previous tab",
            Self::SwitchBook => "Switch book",
            Self::SwitchBookBack => "Switch book, from the oldest",
            Self::BookSwitcher => "Book switcher",
            Self::Palette => "Command palette",
            Self::Settings => "Settings…",
            Self::KeyboardShortcuts => "Keyboard shortcuts",
            Self::NewTab => "New tab",
            Self::ReadingActivity => "Reading activity",
            Self::Library => "Library",
            Self::LibraryImport => "Import a folder into the library",
            Self::LibraryAddByIdentifier => "Add to the library by DOI, arXiv id or ISBN",
            Self::LibraryEnrichAll => "Run the AI pass over the library",
            Self::LibraryReviewAi => "Review the AI's tags and collections",
            Self::LibraryImportZotero => "Import from Zotero",
            Self::LibraryMove => "Move the library to another folder",
            Self::LibraryPauseJobs => "Pause or resume the library's AI pass",
            Self::LibraryOpenNote => "Open the document's note",
            Self::SidebarItem => "Show the library item in the sidebar",
            Self::Home => "Home",
            Self::GamepadMode => "Gamepad mode",
            Self::GamepadMenu => "Gamepad mode's menu",
            Self::ToggleTransparency => "Toggle transparency",
            Self::UiScaleUp => "Larger interface",
            Self::UiScaleDown => "Smaller interface",
            Self::UiScaleReset => "Reset interface size",
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
            Self::FitPage => "Fit page",
            Self::GoToPage => "Go to page…",
            Self::RotateClockwise => "Rotate clockwise",
            Self::RotateCounterclockwise => "Rotate counterclockwise",
            Self::ToggleSpread => "Two-page spread",
            Self::Reload => "Reload the document",
            Self::Present => "Present",
            Self::ToggleFullscreen => "Toggle full screen",
            Self::Zen => "Zen mode: hide everything but the pages",
            Self::PageOverview => "Page overview",
            Self::Outline => "Contents",
            Self::PageColumns => "Pages in one or two columns",
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
            Self::Bookmark => "Bookmark this page, or remove its bookmark",
            Self::TranslateSelection => "Translate the selection",
            Self::TranslateDocument => "Translate the document",
            Self::ShowTranslated => "Show translated or original pages",
            Self::Assistant => "Show or hide the assistant",
            Self::QuickAsk => "Quick ask about this page",
            Self::McpConfig => "Copy the MCP server config for coding agents",
            Self::PresentDraw => "Draw",
            Self::PresentHighlight => "Highlight",
            Self::PresentErase => "Eraser",
            Self::PresentClear => "Clear the page's drawing",
            Self::PresentUndo => "Undo drawing",
            Self::PresentRedo => "Redo drawing",
            Self::PresentCursor => "Show cursor",
            Self::PresentBlack => "Black screen",
            Self::PresentWhite => "White screen",
            Self::PresentTimer => "Start the timer",
            Self::PresentTimerStop => "Stop the timer",
        }
    }

    /// Its Lucide icon, shown in the palette, menus and the dock.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Open => "file-search",
            Self::OpenWithDialog => "folder-open",
            Self::CloseTab => "x",
            Self::NextTab => "chevron-right",
            Self::PreviousTab => "chevron-left",
            Self::SwitchBook | Self::SwitchBookBack => "arrow-left-right",
            Self::BookSwitcher => "book-open",
            Self::Palette => "command",
            Self::Settings => "settings",
            Self::KeyboardShortcuts => "keyboard",
            Self::NewTab => "plus",
            Self::ReadingActivity => "activity",
            Self::Library => "library",
            Self::LibraryImport => "folder-open",
            Self::LibraryAddByIdentifier => "file-plus",
            Self::LibraryEnrichAll => "sparkles",
            Self::LibraryReviewAi => "inbox",
            Self::LibraryImportZotero => "download",
            Self::LibraryMove => "folder-open",
            Self::LibraryPauseJobs => "pause",
            Self::LibraryOpenNote => "file-text",
            Self::SidebarItem => "library-big",
            Self::Home => "house",
            Self::GamepadMode | Self::GamepadMenu => "gamepad-2",
            Self::ToggleTransparency => "eye",
            Self::UiScaleUp => "a-arrow-up",
            Self::UiScaleDown => "a-arrow-down",
            Self::UiScaleReset => "rotate-ccw",
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
            Self::FitPage => "maximize-2",
            Self::GoToPage => "hash",
            Self::RotateClockwise => "rotate-cw",
            Self::RotateCounterclockwise => "rotate-ccw",
            Self::ToggleSpread => "book-open",
            Self::Reload => "refresh-cw",
            Self::Present => "presentation",
            Self::ToggleFullscreen => "expand",
            Self::Zen => "eye-off",
            Self::PageOverview => "layout-grid",
            Self::Outline => "list-tree",
            Self::PageColumns => "rows-2",
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
            Self::McpConfig => "copy",
            Self::PresentDraw => "pencil",
            Self::PresentHighlight => "highlighter",
            Self::PresentErase => "eraser",
            Self::PresentClear => "trash-2",
            Self::PresentUndo => "undo-2",
            Self::PresentRedo => "redo-2",
            Self::PresentCursor => "mouse-pointer-2",
            Self::PresentBlack => "square",
            Self::PresentWhite => "square-dashed",
            Self::PresentTimer => "timer",
            Self::PresentTimerStop => "circle-stop",
        }
    }

    pub fn category(self) -> Category {
        match self {
            Self::Open
            | Self::OpenWithDialog
            | Self::CloseTab
            | Self::NextTab
            | Self::PreviousTab
            | Self::SwitchBook
            | Self::SwitchBookBack
            | Self::BookSwitcher
            | Self::Palette
            | Self::Settings
            | Self::KeyboardShortcuts
            | Self::NewTab
            | Self::ReadingActivity
            | Self::Library
            | Self::LibraryImport
            | Self::LibraryAddByIdentifier
            | Self::LibraryEnrichAll
            | Self::LibraryReviewAi
            | Self::LibraryImportZotero
            | Self::LibraryMove
            | Self::LibraryPauseJobs
            | Self::LibraryOpenNote
            | Self::SidebarItem
            | Self::Home
            | Self::GamepadMode
            | Self::GamepadMenu
            | Self::ToggleSidebar
            | Self::ToggleStats
            | Self::ThemeSystem
            | Self::ThemeLight
            | Self::ThemeDark
            | Self::ToggleTransparency
            | Self::UiScaleUp
            | Self::UiScaleDown
            | Self::UiScaleReset
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
            | Self::FitPage
            | Self::GoToPage
            | Self::RotateClockwise
            | Self::RotateCounterclockwise
            | Self::ToggleSpread
            | Self::Reload
            | Self::Present
            | Self::ToggleFullscreen
            | Self::Zen
            | Self::PageOverview
            | Self::Outline
            | Self::PageColumns
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
            Self::Assistant | Self::QuickAsk | Self::McpConfig => Category::Assistant,
            Self::PresentDraw
            | Self::PresentHighlight
            | Self::PresentErase
            | Self::PresentClear
            | Self::PresentUndo
            | Self::PresentRedo
            | Self::PresentCursor
            | Self::PresentBlack
            | Self::PresentWhite
            | Self::PresentTimer
            | Self::PresentTimerStop => Category::Present,
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
            Self::OpenWithDialog => &["Ctrl+Shift+O"],
            // Ctrl+T translates the selection.
            Self::NewTab => &["Ctrl+N"],
            Self::CloseTab => &["Ctrl+W"],
            Self::NextTab => &["Ctrl+PageDown"],
            Self::PreviousTab => &["Ctrl+PageUp"],
            Self::SwitchBook => &["Ctrl+Tab"],
            Self::SwitchBookBack => &["Ctrl+Shift+Tab"],
            Self::BookSwitcher => &[],
            Self::Palette => &["Ctrl+K", "Ctrl+Shift+P"],
            Self::Settings => &["Ctrl+Comma"],
            Self::ToggleSidebar => &["F9"],
            Self::ToggleStats => &["F12"],
            Self::UiScaleUp => &["Ctrl+Plus", "Ctrl+Equals"],
            Self::UiScaleDown => &["Ctrl+Minus"],
            Self::UiScaleReset => &["Ctrl+0"],
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
            Self::ZoomIn => &["Plus"],
            Self::ZoomOut => &["Minus"],
            Self::FitWidth => &["0", "Equals"],
            Self::RotateClockwise => &["R"],
            Self::RotateCounterclockwise => &["Shift+R"],
            Self::Reload => &["F5", "Ctrl+R"],
            Self::Present => &["P"],
            Self::ToggleFullscreen => &["F11"],
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
            // Presenting's keys, after PowerPoint (E, B, W) and Google
            // Slides (L); they shadow the others only while presenting.
            Self::PresentDraw => &["D"],
            Self::PresentHighlight => &["H"],
            Self::PresentErase => &["X"],
            Self::PresentClear => &["E"],
            Self::PresentUndo => &["Ctrl+Z"],
            Self::PresentRedo => &["Ctrl+Shift+Z", "Ctrl+Y"],
            Self::PresentCursor => &["L"],
            Self::PresentBlack => &["B", "Period"],
            Self::PresentWhite => &["W", "Comma"],
            Self::PresentTimer => &["T"],
            Self::PresentTimerStop => &["Shift+T"],
            Self::ThemeSystem
            | Self::ThemeLight
            | Self::ThemeDark
            | Self::KeyboardShortcuts
            | Self::ReadingActivity
            | Self::Library
            | Self::LibraryImport
            | Self::LibraryAddByIdentifier
            | Self::LibraryEnrichAll
            | Self::LibraryReviewAi
            | Self::LibraryImportZotero
            | Self::LibraryMove
            | Self::LibraryPauseJobs
            | Self::LibraryOpenNote
            | Self::SidebarItem
            | Self::Home
            | Self::ToggleTransparency
            | Self::KeysStandard
            | Self::KeysVim
            | Self::LargerText
            | Self::SmallerText
            | Self::TranslateDocument
            | Self::ShowTranslated
            | Self::SplitDown
            | Self::ClosePane
            | Self::ToggleSpread
            | Self::PageColumns
            | Self::Outline
            | Self::FitPage
            | Self::GoToPage
            | Self::GamepadMode
            | Self::GamepadMenu
            | Self::McpConfig => &[],
        }
    }

    /// After the GPUI Reader's vim preset, zathura and sioyek: Ctrl+W, F, D
    /// and O go to vim's meanings, so closing a tab, Find, Bookmark and Open
    /// move to Q, /, M and O.
    fn vim_keys(self) -> &'static [&'static str] {
        match self {
            Self::Open => &["O"],
            Self::OpenWithDialog => &["Shift+O"],
            Self::CloseTab => &["Q"],
            Self::NextTab => &["G T", "Ctrl+PageDown"],
            Self::PreviousTab => &["G Shift+T", "Ctrl+PageUp"],
            Self::SwitchBook => &["Ctrl+Tab"],
            Self::SwitchBookBack => &["Ctrl+Shift+Tab"],
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

    /// The pad buttons that run it unless the user changed them, in
    /// `pad_keys` notation; `couch` for gamepad mode, where start opens its
    /// menu rather than presenting. The sticks, the triggers and B are
    /// fixed.
    pub fn default_pad(self, couch: bool) -> &'static [&'static str] {
        match self {
            Self::Present if couch => &[],
            Self::GamepadMenu if couch => &["start"],
            Self::GamepadMode => &["select+start"],
            Self::BookSwitcher => &["select+right"],
            Self::NextPage => &["rb", "right"],
            Self::PreviousPage => &["lb", "left"],
            Self::ScrollDown => &["down"],
            Self::ScrollUp => &["up"],
            Self::FirstPage => &["select+lb"],
            Self::LastPage => &["select+rb"],
            Self::FitWidth => &["south"],
            Self::Bookmark => &["west"],
            Self::PageOverview => &["north"],
            Self::Outline => &["select"],
            Self::Present => &["start"],
            _ => &[],
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
            | Self::OpenWithDialog
            | Self::NextTab
            | Self::PreviousTab
            | Self::SwitchBook
            | Self::SwitchBookBack
            | Self::BookSwitcher
            | Self::Palette
            | Self::Settings
            | Self::KeyboardShortcuts
            | Self::NewTab
            | Self::ReadingActivity
            | Self::Library
            | Self::LibraryImport
            | Self::LibraryAddByIdentifier
            | Self::LibraryEnrichAll
            | Self::LibraryReviewAi
            | Self::LibraryImportZotero
            | Self::LibraryMove
            | Self::LibraryPauseJobs
            | Self::Home
            | Self::GamepadMode
            | Self::GamepadMenu
            | Self::ToggleSidebar
            | Self::ToggleStats
            | Self::ThemeSystem
            | Self::ThemeLight
            | Self::ThemeDark
            | Self::ToggleTransparency
            | Self::UiScaleUp
            | Self::UiScaleDown
            | Self::UiScaleReset
            | Self::KeysStandard
            | Self::KeysVim
            | Self::PageColumns
            | Self::ToggleFullscreen
            | Self::McpConfig => Needs::Nothing,
            Self::LargerText | Self::SmallerText => Needs::Reflow,
            Self::TranslateSelection => Needs::Selection,
            Self::ShowTranslated => Needs::Translation,
            Self::FocusNextPane | Self::ClosePane => Needs::Panes,
            Self::RotateClockwise
            | Self::RotateCounterclockwise
            | Self::ToggleSpread
            | Self::RegionSelect => Needs::FixedLayout,
            command if command.category() == Category::Present => Needs::Presenting,
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
                | Self::FitPage
                | Self::RotateClockwise
                | Self::RotateCounterclockwise
                | Self::Present
                | Self::Zen
        ) && self.category() != Category::Present
    }
}

/// Icons the app names directly rather than through a command, section or
/// sidebar tab; a test checks that each exists.
#[cfg(test)]
pub const ICONS_IN_USE: &[&str] = &[
    "house",
    "folder-search",
    "folder",
    "library",
    "monitor",
    "sun",
    "moon",
    "keyboard",
    "arrow-right",
    "file-text",
    "book-open",
    "book-open-text",
    "plus",
    "x",
    "chevron-down",
    "chevron-left",
    "chevron-right",
    "ellipsis",
    "search",
    "send",
    "message-square-plus",
    "clock",
    "copy",
    "trash-2",
    "check",
    "eye",
    "eye-off",
    "rotate-ccw",
    "refresh-cw",
    "download",
    "file-down",
    "pin",
    "triangle-alert",
    "info",
    "gamepad-2",
    "library",
    "pen-line",
    "highlighter",
    "sticky-note",
    "arrow-up",
    "arrow-down",
    "log-in",
    "log-out",
    "columns-2",
    "wand-sparkles",
    "image",
    "folder-open",
    "file-plus",
    "at-sign",
    "paperclip",
    "brain",
    "maximize-2",
    "minimize-2",
    "pencil",
    "circle-stop",
    "list-todo",
    "bot",
    "terminal",
    "search-check",
    "corner-down-left",
    "text-cursor",
    "undo-2",
    "redo-2",
    "circle-alert",
    "wrench",
    "chevrons-up-down",
    "file-search",
    "play",
    "pause",
    "shrink",
    "expand",
    "eraser",
    "mouse-pointer-2",
    "square",
];

#[cfg(test)]
mod tests {
    #[test]
    fn every_icon_in_use_exists() {
        use crate::{settings_window::Section, sidebar::SidebarTab};
        let names = super::Command::ALL
            .iter()
            .map(|command| command.icon())
            .chain(Section::ALL.iter().map(|section| section.icon()))
            .chain(SidebarTab::ALL.iter().map(|tab| tab.icon()))
            .chain(super::ICONS_IN_USE.iter().copied());
        for name in names {
            assert!(crate::icons::exists(name), "missing icon {name}");
        }
    }

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
