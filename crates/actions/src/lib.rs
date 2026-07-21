pub mod ai_panel {
    gpui::actions!(ai_panel, [AiPanelClose, AiPanelSubmit]);
}

pub mod input {
    gpui::actions!(
        input,
        [
            Backspace,
            Delete,
            DeleteWordBackward,
            DeleteWordForward,
            DeleteToLineStart,
            DeleteToLineEnd,
            Left,
            Right,
            WordLeft,
            WordRight,
            Up,
            Down,
            Home,
            End,
            DocumentStart,
            DocumentEnd,
            SelectAll,
            SelectLeft,
            SelectRight,
            SelectWordLeft,
            SelectWordRight,
            SelectUp,
            SelectDown,
            SelectHome,
            SelectEnd,
            SelectDocumentStart,
            SelectDocumentEnd,
            Copy,
            Cut,
            Paste,
            Undo,
            Redo,
            Newline
        ]
    );
}

pub mod markdown {
    gpui::actions!(markdown, [Copy]);
}
pub mod menu {
    gpui::actions!(menu, [Confirm, Cancel, SelectNext, SelectPrev, Remove]);
}
pub mod new_tab {
    gpui::actions!(new_tab, [EnterSelect, CancelSelect, DeleteSelected]);
}
pub mod note_editor {
    gpui::actions!(note_editor, [NoteSave, NoteCancel]);
}
pub mod settings_panel {
    gpui::actions!(settings_panel, [PanelClose]);
}

pub mod pane {
    gpui::actions!(
        pane,
        [
            SplitLeft,
            SplitRight,
            SplitUp,
            SplitDown,
            SplitVertical,
            SplitHorizontal,
        ]
    );
}

pub mod search {
    gpui::actions!(
        search,
        [SearchInputConfirm, SearchInputCancel, SearchInputPrevious]
    );
}

pub mod pdf_viewer {
    gpui::actions!(
        pdf_viewer,
        [
            CopySelection,
            Undo,
            Redo,
            SelectAll,
            ZoomIn,
            ZoomOut,
            ResetZoom,
            RotateClockwise,
            RotateCounterclockwise,
            TogglePublisherStyles,
            Reload,
            Search,
            NextSearchResult,
            PrevSearchResult,
            NextPage,
            PreviousPage,
            NextHalfPage,
            PreviousHalfPage,
            ScrollLineDown,
            ScrollLineUp,
            GoToBeginning,
            GoToEnd,
            JumpBackward,
            JumpForward,
            Toc,
            ToggleToc,
            OpenNotes,
            ToggleNotes,
            ToggleThumbnailSidebar,
            ToggleCommandPalette,
            ToggleFullScreen,
            TogglePresentationMode,
            ToggleTransparency,
            OpenSettings,
            OpenKeybindings,
            ToggleAiPanel,
            OpenHistory,
            OpenBookmarks,
            OpenFile,
            OpenNewTab,
            OpenQuickAsk,
            OpenGoToPage,
            CloseTab,
            AddBookmark,
            ToggleGridOverview,
            GridCursorUp,
            GridCursorDown,
            GridCursorLeft,
            GridCursorRight,
            GridOverviewConfirm,
            TranslateSelection,
            TranslateDocument,
            CancelDocumentTranslation,
            TogglePinnedTranslation,
            OpenTranslationSplit,
            ToggleRegionSelection,
            CancelRegionSelection,
        ]
    );
}

pub mod workspace {
    gpui::actions!(
        workspace,
        [
            ActivatePaneLeft,
            ActivatePaneRight,
            ActivatePaneUp,
            ActivatePaneDown,
            ActivateNextPane,
            ActivatePreviousPane,
            SwapPaneLeft,
            SwapPaneRight,
            SwapPaneUp,
            SwapPaneDown,
            MovePaneLeft,
            MovePaneRight,
            MovePaneUp,
            MovePaneDown,
            ResizePaneLeft,
            ResizePaneRight,
            ResizePaneUp,
            ResizePaneDown,
            CloseOtherPanes,
            ResetPaneSizes,
            ToggleMode,
            ToggleThemeSelector,
        ]
    );
}
