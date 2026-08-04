pub mod ai_panel {
    gpui::actions!(ai_panel, [AiPanelClose]);
}

pub mod menu {
    gpui::actions!(menu, [Confirm, Cancel, SelectNext, SelectPrev, Remove]);
}
pub mod new_tab {
    gpui::actions!(
        new_tab,
        [
            ActivateFocused,
            CancelSelect,
            CursorDown,
            CursorFirst,
            CursorLast,
            CursorLeft,
            CursorRight,
            CursorUp,
            DeleteSelected,
            EnterSelect,
            ExtendSelectionDown,
            ExtendSelectionFirst,
            ExtendSelectionLast,
            ExtendSelectionLeft,
            ExtendSelectionRight,
            ExtendSelectionUp,
            SelectAll,
            ToggleFocused,
        ]
    );
}
pub mod note_editor {
    gpui::actions!(note_editor, [NoteCancel]);
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

pub mod document_viewer {
    gpui::actions!(
        document_viewer,
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
            OpenWithSystemPicker,
            OpenNewTab,
            OpenQuickAsk,
            OpenGoToPage,
            CloseTab,
            DuplicateTab,
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
