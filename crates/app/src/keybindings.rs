use std::rc::Rc;

use crate::{
    ai_panel::{
        AiPanelBackspace, AiPanelClose, AiPanelCopy, AiPanelCut, AiPanelDelete, AiPanelDown,
        AiPanelEnd, AiPanelHome, AiPanelLeft, AiPanelNewline, AiPanelPaste, AiPanelRight,
        AiPanelSelectAll, AiPanelSelectDown, AiPanelSelectEnd, AiPanelSelectHome,
        AiPanelSelectLeft, AiPanelSelectRight, AiPanelSelectUp, AiPanelSubmit, AiPanelUp,
    },
    note_editor::{
        NoteBackspace, NoteCancel, NoteCopy, NoteCut, NoteDelete, NoteDown, NoteEnd, NoteHome,
        NoteLeft, NoteNewline, NotePaste, NoteRight, NoteSave, NoteSelectAll, NoteSelectDown,
        NoteSelectEnd, NoteSelectHome, NoteSelectLeft, NoteSelectRight, NoteSelectUp, NoteUp,
    },
    pdf_view::{
        AddBookmark, CloseTab, CopySelection, GoToBeginning, GoToEnd, GridCursorDown,
        GridCursorLeft, GridCursorRight, GridCursorUp, GridOverviewConfirm, JumpBackward,
        JumpForward, NextHalfPage, NextPage, OpenBookmarks, OpenFile, OpenHistory, OpenNewTab,
        OpenNotes, OpenSettings, PreviousHalfPage, PreviousPage, Reload, ResetZoom, Search,
        SearchNext, SearchPrev, SelectAll, Toc, ToggleAiPanel, ToggleCommandPalette,
        ToggleFullScreen, ToggleGridOverview, ToggleNotes, ToggleThumbnailSidebar, ToggleToc,
        ToggleTransparency, ZoomIn, ZoomOut,
    },
    picker::{
        PickerBackspace, PickerCancel, PickerConfirm, PickerCopy, PickerCut, PickerDelete,
        PickerEnd, PickerHome, PickerLeft, PickerPaste, PickerRemove, PickerRight, PickerSelectAll,
        PickerSelectEnd, PickerSelectHome, PickerSelectLeft, PickerSelectNext, PickerSelectPrev,
        PickerSelectRight,
    },
    search::{
        SearchInputBackspace, SearchInputCancel, SearchInputConfirm, SearchInputCopy,
        SearchInputCut, SearchInputDelete, SearchInputEnd, SearchInputHome, SearchInputLeft,
        SearchInputPaste, SearchInputPrevious, SearchInputRight, SearchInputSelectAll,
        SearchInputSelectEnd, SearchInputSelectHome, SearchInputSelectLeft, SearchInputSelectRight,
    },
    settings::{KeyBindingOverride, KeymapPreset},
    settings_window::{
        PanelClose, SettingsInputBackspace, SettingsInputCopy, SettingsInputCut,
        SettingsInputDelete, SettingsInputEnd, SettingsInputHome, SettingsInputLeft,
        SettingsInputPaste, SettingsInputRight, SettingsInputSelectAll, SettingsInputSelectEnd,
        SettingsInputSelectHome, SettingsInputSelectLeft, SettingsInputSelectRight,
    },
};
use gpui::{Action, App, KeyBinding, KeyBindingContextPredicate};

const PDF_CONTEXT: &str = "PdfView && !GridOverview && !CommandPalette && !Search && !Toc && !Notes && !History && !Bookmarks && !BookmarkPrompt && !NoteEditor && !AiPanel && !SettingsPanel";
const PDF_NO_SEARCH_CONTEXT: &str = "PdfView && !GridOverview && !Search && !Toc && !Notes && !History && !Bookmarks && !BookmarkPrompt && !NoteEditor && !AiPanel && !SettingsPanel";
const WORKSPACE_CONTEXT: &str = "Workspace && !GridOverview && !CommandPalette && !Search && !Toc && !Notes && !History && !Bookmarks && !BookmarkPrompt && !NoteEditor && !AiPanel && !SettingsPanel";
const COMMAND_PALETTE_CONTEXT: &str = "Workspace && !Search && !Toc && !Notes && !History && !Bookmarks && !BookmarkPrompt && !NoteEditor && !AiPanel && !SettingsPanel";
const GRID_OVERVIEW_CONTEXT: &str = "GridOverview && !SettingsPanel";
const PALETTE_CONTEXT: &str = "CommandPalette";
const SEARCH_CONTEXT: &str = "Search";
const SETTINGS_CONTEXT: &str = "SettingsPanel";
const SETTINGS_INPUT_CONTEXT: &str = "SettingsInput";
const AI_PANEL_CONTEXT: &str = "AiPanel";
const TOC_CONTEXT: &str = "Toc";
const NOTES_CONTEXT: &str = "Notes";
const HISTORY_CONTEXT: &str = "History";
const BOOKMARKS_CONTEXT: &str = "Bookmarks";
const BOOKMARK_PROMPT_CONTEXT: &str = "BookmarkPrompt";
const NOTE_EDITOR_CONTEXT: &str = "NoteEditor";

struct ActionSpec {
    aliases: &'static [&'static str],
    context: Option<&'static str>,
    build: fn() -> Box<dyn Action>,
}

pub fn apply(cx: &mut App, preset: KeymapPreset, overrides: &[KeyBindingOverride]) {
    let mut bindings = base_bindings(preset);
    for binding in overrides {
        bindings.extend(bindings_for_override(cx, binding));
    }

    cx.clear_key_bindings();
    cx.bind_keys(bindings);
}

fn base_bindings(preset: KeymapPreset) -> Vec<KeyBinding> {
    let mut bindings = Vec::new();

    bindings.extend([
        bind::<CopySelection>("ctrl-c", PDF_CONTEXT),
        bind::<CopySelection>("cmd-c", PDF_CONTEXT),
        bind::<SelectAll>("ctrl-a", PDF_CONTEXT),
        bind::<SelectAll>("cmd-a", PDF_CONTEXT),
        bind::<ZoomIn>("+", PDF_CONTEXT),
        bind::<ZoomIn>("shift-=", PDF_CONTEXT),
        bind::<ZoomOut>("-", PDF_CONTEXT),
        bind::<ResetZoom>("=", PDF_CONTEXT),
        bind::<Reload>("f5", PDF_CONTEXT),
        bind::<GoToBeginning>("home", PDF_CONTEXT),
        bind::<GoToEnd>("end", PDF_CONTEXT),
        bind::<GoToBeginning>("ctrl-home", PDF_CONTEXT),
        bind::<GoToEnd>("ctrl-end", PDF_CONTEXT),
        bind::<GoToBeginning>("alt-<", PDF_CONTEXT),
        bind::<GoToEnd>("alt->", PDF_CONTEXT),
        bind::<ToggleCommandPalette>("ctrl-shift-p", COMMAND_PALETTE_CONTEXT),
        bind::<ToggleCommandPalette>("cmd-shift-p", COMMAND_PALETTE_CONTEXT),
        bind::<ToggleFullScreen>("f11", PDF_NO_SEARCH_CONTEXT),
        bind::<ToggleTransparency>("ctrl-shift-t", PDF_CONTEXT),
        bind::<ToggleTransparency>("cmd-shift-t", PDF_CONTEXT),
        bind::<OpenSettings>("ctrl-,", PDF_NO_SEARCH_CONTEXT),
        bind::<OpenSettings>("cmd-,", PDF_NO_SEARCH_CONTEXT),
        bind::<ToggleAiPanel>("ctrl-shift-a", PDF_NO_SEARCH_CONTEXT),
        bind::<ToggleAiPanel>("cmd-shift-a", PDF_NO_SEARCH_CONTEXT),
        bind::<OpenHistory>("ctrl-h", PDF_NO_SEARCH_CONTEXT),
        bind::<ToggleThumbnailSidebar>("ctrl-shift-g", PDF_NO_SEARCH_CONTEXT),
        bind::<ToggleThumbnailSidebar>("cmd-shift-g", PDF_NO_SEARCH_CONTEXT),
        bind::<OpenFile>("cmd-o", WORKSPACE_CONTEXT),
        bind::<OpenNewTab>("cmd-t", WORKSPACE_CONTEXT),
        bind::<OpenNewTab>("ctrl-t", WORKSPACE_CONTEXT),
        bind::<CloseTab>("cmd-w", WORKSPACE_CONTEXT),
        bind::<CloseTab>("ctrl-w", WORKSPACE_CONTEXT),
        bind::<ToggleGridOverview>("ctrl-g", PDF_NO_SEARCH_CONTEXT),
        bind::<ZoomIn>("+", GRID_OVERVIEW_CONTEXT),
        bind::<ZoomIn>("shift-=", GRID_OVERVIEW_CONTEXT),
        bind::<ZoomOut>("-", GRID_OVERVIEW_CONTEXT),
        bind::<GridCursorUp>("up", GRID_OVERVIEW_CONTEXT),
        bind::<GridCursorDown>("down", GRID_OVERVIEW_CONTEXT),
        bind::<GridCursorLeft>("left", GRID_OVERVIEW_CONTEXT),
        bind::<GridCursorRight>("right", GRID_OVERVIEW_CONTEXT),
        bind::<GoToBeginning>("home", GRID_OVERVIEW_CONTEXT),
        bind::<GoToEnd>("end", GRID_OVERVIEW_CONTEXT),
        bind::<GoToBeginning>("ctrl-home", GRID_OVERVIEW_CONTEXT),
        bind::<GoToEnd>("ctrl-end", GRID_OVERVIEW_CONTEXT),
        bind::<GoToBeginning>("alt-<", GRID_OVERVIEW_CONTEXT),
        bind::<GoToEnd>("alt->", GRID_OVERVIEW_CONTEXT),
        bind::<GridOverviewConfirm>("enter", GRID_OVERVIEW_CONTEXT),
        bind::<ToggleGridOverview>("ctrl-g", GRID_OVERVIEW_CONTEXT),
        bind::<ToggleGridOverview>("escape", GRID_OVERVIEW_CONTEXT),
    ]);

    match preset {
        KeymapPreset::Modern => bindings.extend([
            bind::<OpenFile>("ctrl-o", WORKSPACE_CONTEXT),
            bind::<Search>("ctrl-f", PDF_CONTEXT),
            bind::<Search>("cmd-f", PDF_CONTEXT),
            bind::<Search>("/", PDF_CONTEXT),
            bind::<SearchNext>("f3", PDF_CONTEXT),
            bind::<SearchPrev>("shift-f3", PDF_CONTEXT),
            bind::<NextPage>("pagedown", PDF_CONTEXT),
            bind::<PreviousPage>("pageup", PDF_CONTEXT),
            bind::<NextHalfPage>("shift-pagedown", PDF_CONTEXT),
            bind::<PreviousHalfPage>("shift-pageup", PDF_CONTEXT),
            bind::<NextPage>("pagedown", GRID_OVERVIEW_CONTEXT),
            bind::<PreviousPage>("pageup", GRID_OVERVIEW_CONTEXT),
            bind::<NextHalfPage>("shift-pagedown", GRID_OVERVIEW_CONTEXT),
            bind::<PreviousHalfPage>("shift-pageup", GRID_OVERVIEW_CONTEXT),
            bind::<OpenBookmarks>("ctrl-b", PDF_NO_SEARCH_CONTEXT),
            bind::<AddBookmark>("ctrl-d", PDF_CONTEXT),
            bind::<Reload>("ctrl-r", PDF_CONTEXT),
        ]),
        KeymapPreset::Vim => bindings.extend([
            bind::<Search>("/", PDF_CONTEXT),
            bind::<SearchNext>("n", PDF_CONTEXT),
            bind::<SearchPrev>("shift-n", PDF_CONTEXT),
            bind::<NextPage>("ctrl-f", PDF_CONTEXT),
            bind::<PreviousPage>("ctrl-b", PDF_CONTEXT),
            bind::<NextHalfPage>("ctrl-d", PDF_CONTEXT),
            bind::<PreviousHalfPage>("ctrl-u", PDF_CONTEXT),
            bind::<GoToBeginning>("g g", PDF_CONTEXT),
            bind::<GoToEnd>("shift-g", PDF_CONTEXT),
            bind::<NextPage>("ctrl-f", GRID_OVERVIEW_CONTEXT),
            bind::<PreviousPage>("ctrl-b", GRID_OVERVIEW_CONTEXT),
            bind::<NextHalfPage>("ctrl-d", GRID_OVERVIEW_CONTEXT),
            bind::<PreviousHalfPage>("ctrl-u", GRID_OVERVIEW_CONTEXT),
            bind::<GoToBeginning>("g g", GRID_OVERVIEW_CONTEXT),
            bind::<GoToEnd>("shift-g", GRID_OVERVIEW_CONTEXT),
            bind::<JumpBackward>("ctrl-o", PDF_CONTEXT),
            bind::<JumpForward>("ctrl-i", PDF_CONTEXT),
            bind::<OpenBookmarks>("'", PDF_NO_SEARCH_CONTEXT),
            bind::<AddBookmark>("m", PDF_CONTEXT),
            bind::<Reload>("r", PDF_CONTEXT),
        ]),
    }

    bindings.extend([
        bind::<PickerConfirm>("enter", PALETTE_CONTEXT),
        bind::<PickerCancel>("escape", PALETTE_CONTEXT),
        bind::<PickerSelectNext>("down", PALETTE_CONTEXT),
        bind::<PickerSelectPrev>("up", PALETTE_CONTEXT),
        bind::<PickerBackspace>("backspace", PALETTE_CONTEXT),
        bind::<PickerDelete>("delete", PALETTE_CONTEXT),
        bind::<PickerLeft>("left", PALETTE_CONTEXT),
        bind::<PickerRight>("right", PALETTE_CONTEXT),
        bind::<PickerHome>("home", PALETTE_CONTEXT),
        bind::<PickerEnd>("end", PALETTE_CONTEXT),
        bind::<PickerSelectAll>("ctrl-a", PALETTE_CONTEXT),
        bind::<PickerSelectAll>("cmd-a", PALETTE_CONTEXT),
        bind::<PickerSelectLeft>("shift-left", PALETTE_CONTEXT),
        bind::<PickerSelectRight>("shift-right", PALETTE_CONTEXT),
        bind::<PickerSelectHome>("shift-home", PALETTE_CONTEXT),
        bind::<PickerSelectEnd>("shift-end", PALETTE_CONTEXT),
        bind::<PickerCopy>("ctrl-c", PALETTE_CONTEXT),
        bind::<PickerCopy>("cmd-c", PALETTE_CONTEXT),
        bind::<PickerCut>("ctrl-x", PALETTE_CONTEXT),
        bind::<PickerCut>("cmd-x", PALETTE_CONTEXT),
        bind::<PickerPaste>("ctrl-v", PALETTE_CONTEXT),
        bind::<PickerPaste>("cmd-v", PALETTE_CONTEXT),
        bind::<SearchInputConfirm>("enter", SEARCH_CONTEXT),
        bind::<SearchInputPrevious>("shift-enter", SEARCH_CONTEXT),
        bind::<SearchInputCancel>("escape", SEARCH_CONTEXT),
        bind::<SearchInputBackspace>("backspace", SEARCH_CONTEXT),
        bind::<SearchInputDelete>("delete", SEARCH_CONTEXT),
        bind::<SearchInputLeft>("left", SEARCH_CONTEXT),
        bind::<SearchInputRight>("right", SEARCH_CONTEXT),
        bind::<SearchInputHome>("home", SEARCH_CONTEXT),
        bind::<SearchInputEnd>("end", SEARCH_CONTEXT),
        bind::<SearchInputSelectAll>("ctrl-a", SEARCH_CONTEXT),
        bind::<SearchInputSelectAll>("cmd-a", SEARCH_CONTEXT),
        bind::<SearchInputSelectLeft>("shift-left", SEARCH_CONTEXT),
        bind::<SearchInputSelectRight>("shift-right", SEARCH_CONTEXT),
        bind::<SearchInputSelectHome>("shift-home", SEARCH_CONTEXT),
        bind::<SearchInputSelectEnd>("shift-end", SEARCH_CONTEXT),
        bind::<SearchInputCopy>("ctrl-c", SEARCH_CONTEXT),
        bind::<SearchInputCopy>("cmd-c", SEARCH_CONTEXT),
        bind::<SearchInputCut>("ctrl-x", SEARCH_CONTEXT),
        bind::<SearchInputCut>("cmd-x", SEARCH_CONTEXT),
        bind::<SearchInputPaste>("ctrl-v", SEARCH_CONTEXT),
        bind::<SearchInputPaste>("cmd-v", SEARCH_CONTEXT),
        bind::<PanelClose>("escape", SETTINGS_CONTEXT),
        bind::<SettingsInputBackspace>("backspace", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputDelete>("delete", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputLeft>("left", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputRight>("right", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputHome>("home", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputEnd>("end", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputSelectAll>("ctrl-a", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputSelectAll>("cmd-a", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputSelectLeft>("shift-left", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputSelectRight>("shift-right", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputSelectHome>("shift-home", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputSelectEnd>("shift-end", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputCopy>("ctrl-c", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputCopy>("cmd-c", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputCut>("ctrl-x", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputCut>("cmd-x", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputPaste>("ctrl-v", SETTINGS_INPUT_CONTEXT),
        bind::<SettingsInputPaste>("cmd-v", SETTINGS_INPUT_CONTEXT),
        bind::<PickerConfirm>("enter", TOC_CONTEXT),
        bind::<PickerCancel>("escape", TOC_CONTEXT),
        bind::<PickerSelectNext>("down", TOC_CONTEXT),
        bind::<PickerSelectPrev>("up", TOC_CONTEXT),
        bind::<PickerBackspace>("backspace", TOC_CONTEXT),
        bind::<PickerDelete>("delete", TOC_CONTEXT),
        bind::<PickerLeft>("left", TOC_CONTEXT),
        bind::<PickerRight>("right", TOC_CONTEXT),
        bind::<PickerHome>("home", TOC_CONTEXT),
        bind::<PickerEnd>("end", TOC_CONTEXT),
        bind::<PickerSelectAll>("ctrl-a", TOC_CONTEXT),
        bind::<PickerSelectAll>("cmd-a", TOC_CONTEXT),
        bind::<PickerSelectLeft>("shift-left", TOC_CONTEXT),
        bind::<PickerSelectRight>("shift-right", TOC_CONTEXT),
        bind::<PickerSelectHome>("shift-home", TOC_CONTEXT),
        bind::<PickerSelectEnd>("shift-end", TOC_CONTEXT),
        bind::<PickerCopy>("ctrl-c", TOC_CONTEXT),
        bind::<PickerCopy>("cmd-c", TOC_CONTEXT),
        bind::<PickerCut>("ctrl-x", TOC_CONTEXT),
        bind::<PickerCut>("cmd-x", TOC_CONTEXT),
        bind::<PickerPaste>("ctrl-v", TOC_CONTEXT),
        bind::<PickerPaste>("cmd-v", TOC_CONTEXT),
        bind::<PickerConfirm>("enter", NOTES_CONTEXT),
        bind::<PickerCancel>("escape", NOTES_CONTEXT),
        bind::<PickerSelectNext>("down", NOTES_CONTEXT),
        bind::<PickerSelectPrev>("up", NOTES_CONTEXT),
        bind::<PickerBackspace>("backspace", NOTES_CONTEXT),
        bind::<PickerDelete>("delete", NOTES_CONTEXT),
        bind::<PickerRemove>("shift-delete", NOTES_CONTEXT),
        bind::<PickerLeft>("left", NOTES_CONTEXT),
        bind::<PickerRight>("right", NOTES_CONTEXT),
        bind::<PickerHome>("home", NOTES_CONTEXT),
        bind::<PickerEnd>("end", NOTES_CONTEXT),
        bind::<PickerSelectAll>("ctrl-a", NOTES_CONTEXT),
        bind::<PickerSelectAll>("cmd-a", NOTES_CONTEXT),
        bind::<PickerSelectLeft>("shift-left", NOTES_CONTEXT),
        bind::<PickerSelectRight>("shift-right", NOTES_CONTEXT),
        bind::<PickerSelectHome>("shift-home", NOTES_CONTEXT),
        bind::<PickerSelectEnd>("shift-end", NOTES_CONTEXT),
        bind::<PickerCopy>("ctrl-c", NOTES_CONTEXT),
        bind::<PickerCopy>("cmd-c", NOTES_CONTEXT),
        bind::<PickerCut>("ctrl-x", NOTES_CONTEXT),
        bind::<PickerCut>("cmd-x", NOTES_CONTEXT),
        bind::<PickerPaste>("ctrl-v", NOTES_CONTEXT),
        bind::<PickerPaste>("cmd-v", NOTES_CONTEXT),
        bind::<PickerConfirm>("enter", HISTORY_CONTEXT),
        bind::<PickerCancel>("escape", HISTORY_CONTEXT),
        bind::<PickerSelectNext>("down", HISTORY_CONTEXT),
        bind::<PickerSelectPrev>("up", HISTORY_CONTEXT),
        bind::<PickerBackspace>("backspace", HISTORY_CONTEXT),
        bind::<PickerDelete>("delete", HISTORY_CONTEXT),
        bind::<PickerLeft>("left", HISTORY_CONTEXT),
        bind::<PickerRight>("right", HISTORY_CONTEXT),
        bind::<PickerHome>("home", HISTORY_CONTEXT),
        bind::<PickerEnd>("end", HISTORY_CONTEXT),
        bind::<PickerSelectAll>("ctrl-a", HISTORY_CONTEXT),
        bind::<PickerSelectAll>("cmd-a", HISTORY_CONTEXT),
        bind::<PickerSelectLeft>("shift-left", HISTORY_CONTEXT),
        bind::<PickerSelectRight>("shift-right", HISTORY_CONTEXT),
        bind::<PickerSelectHome>("shift-home", HISTORY_CONTEXT),
        bind::<PickerSelectEnd>("shift-end", HISTORY_CONTEXT),
        bind::<PickerCopy>("ctrl-c", HISTORY_CONTEXT),
        bind::<PickerCopy>("cmd-c", HISTORY_CONTEXT),
        bind::<PickerCut>("ctrl-x", HISTORY_CONTEXT),
        bind::<PickerCut>("cmd-x", HISTORY_CONTEXT),
        bind::<PickerPaste>("ctrl-v", HISTORY_CONTEXT),
        bind::<PickerPaste>("cmd-v", HISTORY_CONTEXT),
        bind::<PickerConfirm>("enter", BOOKMARKS_CONTEXT),
        bind::<PickerCancel>("escape", BOOKMARKS_CONTEXT),
        bind::<PickerSelectNext>("down", BOOKMARKS_CONTEXT),
        bind::<PickerSelectPrev>("up", BOOKMARKS_CONTEXT),
        bind::<PickerBackspace>("backspace", BOOKMARKS_CONTEXT),
        bind::<PickerDelete>("delete", BOOKMARKS_CONTEXT),
        bind::<PickerRemove>("shift-delete", BOOKMARKS_CONTEXT),
        bind::<PickerLeft>("left", BOOKMARKS_CONTEXT),
        bind::<PickerRight>("right", BOOKMARKS_CONTEXT),
        bind::<PickerHome>("home", BOOKMARKS_CONTEXT),
        bind::<PickerEnd>("end", BOOKMARKS_CONTEXT),
        bind::<PickerSelectAll>("ctrl-a", BOOKMARKS_CONTEXT),
        bind::<PickerSelectAll>("cmd-a", BOOKMARKS_CONTEXT),
        bind::<PickerSelectLeft>("shift-left", BOOKMARKS_CONTEXT),
        bind::<PickerSelectRight>("shift-right", BOOKMARKS_CONTEXT),
        bind::<PickerSelectHome>("shift-home", BOOKMARKS_CONTEXT),
        bind::<PickerSelectEnd>("shift-end", BOOKMARKS_CONTEXT),
        bind::<PickerCopy>("ctrl-c", BOOKMARKS_CONTEXT),
        bind::<PickerCopy>("cmd-c", BOOKMARKS_CONTEXT),
        bind::<PickerCut>("ctrl-x", BOOKMARKS_CONTEXT),
        bind::<PickerCut>("cmd-x", BOOKMARKS_CONTEXT),
        bind::<PickerPaste>("ctrl-v", BOOKMARKS_CONTEXT),
        bind::<PickerPaste>("cmd-v", BOOKMARKS_CONTEXT),
        bind::<PickerConfirm>("enter", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerCancel>("escape", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerBackspace>("backspace", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerDelete>("delete", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerLeft>("left", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerRight>("right", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerHome>("home", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerEnd>("end", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerSelectAll>("ctrl-a", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerSelectAll>("cmd-a", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerSelectLeft>("shift-left", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerSelectRight>("shift-right", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerSelectHome>("shift-home", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerSelectEnd>("shift-end", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerCopy>("ctrl-c", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerCopy>("cmd-c", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerCut>("ctrl-x", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerCut>("cmd-x", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerPaste>("ctrl-v", BOOKMARK_PROMPT_CONTEXT),
        bind::<PickerPaste>("cmd-v", BOOKMARK_PROMPT_CONTEXT),
        bind::<NoteSave>("ctrl-enter", NOTE_EDITOR_CONTEXT),
        bind::<NoteSave>("cmd-enter", NOTE_EDITOR_CONTEXT),
        bind::<NoteCancel>("escape", NOTE_EDITOR_CONTEXT),
        bind::<NoteNewline>("enter", NOTE_EDITOR_CONTEXT),
        bind::<NoteBackspace>("backspace", NOTE_EDITOR_CONTEXT),
        bind::<NoteDelete>("delete", NOTE_EDITOR_CONTEXT),
        bind::<NoteLeft>("left", NOTE_EDITOR_CONTEXT),
        bind::<NoteRight>("right", NOTE_EDITOR_CONTEXT),
        bind::<NoteUp>("up", NOTE_EDITOR_CONTEXT),
        bind::<NoteDown>("down", NOTE_EDITOR_CONTEXT),
        bind::<NoteHome>("home", NOTE_EDITOR_CONTEXT),
        bind::<NoteEnd>("end", NOTE_EDITOR_CONTEXT),
        bind::<NoteSelectAll>("ctrl-a", NOTE_EDITOR_CONTEXT),
        bind::<NoteSelectAll>("cmd-a", NOTE_EDITOR_CONTEXT),
        bind::<NoteSelectLeft>("shift-left", NOTE_EDITOR_CONTEXT),
        bind::<NoteSelectRight>("shift-right", NOTE_EDITOR_CONTEXT),
        bind::<NoteSelectUp>("shift-up", NOTE_EDITOR_CONTEXT),
        bind::<NoteSelectDown>("shift-down", NOTE_EDITOR_CONTEXT),
        bind::<NoteSelectHome>("shift-home", NOTE_EDITOR_CONTEXT),
        bind::<NoteSelectEnd>("shift-end", NOTE_EDITOR_CONTEXT),
        bind::<NoteCopy>("ctrl-c", NOTE_EDITOR_CONTEXT),
        bind::<NoteCopy>("cmd-c", NOTE_EDITOR_CONTEXT),
        bind::<NoteCut>("ctrl-x", NOTE_EDITOR_CONTEXT),
        bind::<NoteCut>("cmd-x", NOTE_EDITOR_CONTEXT),
        bind::<NotePaste>("ctrl-v", NOTE_EDITOR_CONTEXT),
        bind::<NotePaste>("cmd-v", NOTE_EDITOR_CONTEXT),
        bind::<AiPanelSubmit>("ctrl-enter", AI_PANEL_CONTEXT),
        bind::<AiPanelSubmit>("cmd-enter", AI_PANEL_CONTEXT),
        bind::<AiPanelClose>("escape", AI_PANEL_CONTEXT),
        bind::<AiPanelNewline>("enter", AI_PANEL_CONTEXT),
        bind::<AiPanelNewline>("shift-enter", AI_PANEL_CONTEXT),
        bind::<AiPanelBackspace>("backspace", AI_PANEL_CONTEXT),
        bind::<AiPanelDelete>("delete", AI_PANEL_CONTEXT),
        bind::<AiPanelLeft>("left", AI_PANEL_CONTEXT),
        bind::<AiPanelRight>("right", AI_PANEL_CONTEXT),
        bind::<AiPanelUp>("up", AI_PANEL_CONTEXT),
        bind::<AiPanelDown>("down", AI_PANEL_CONTEXT),
        bind::<AiPanelHome>("home", AI_PANEL_CONTEXT),
        bind::<AiPanelEnd>("end", AI_PANEL_CONTEXT),
        bind::<AiPanelSelectAll>("ctrl-a", AI_PANEL_CONTEXT),
        bind::<AiPanelSelectAll>("cmd-a", AI_PANEL_CONTEXT),
        bind::<AiPanelSelectLeft>("shift-left", AI_PANEL_CONTEXT),
        bind::<AiPanelSelectRight>("shift-right", AI_PANEL_CONTEXT),
        bind::<AiPanelSelectUp>("shift-up", AI_PANEL_CONTEXT),
        bind::<AiPanelSelectDown>("shift-down", AI_PANEL_CONTEXT),
        bind::<AiPanelSelectHome>("shift-home", AI_PANEL_CONTEXT),
        bind::<AiPanelSelectEnd>("shift-end", AI_PANEL_CONTEXT),
        bind::<AiPanelCopy>("ctrl-c", AI_PANEL_CONTEXT),
        bind::<AiPanelCopy>("cmd-c", AI_PANEL_CONTEXT),
        bind::<AiPanelCut>("ctrl-x", AI_PANEL_CONTEXT),
        bind::<AiPanelCut>("cmd-x", AI_PANEL_CONTEXT),
        bind::<AiPanelPaste>("ctrl-v", AI_PANEL_CONTEXT),
        bind::<AiPanelPaste>("cmd-v", AI_PANEL_CONTEXT),
    ]);

    bindings
}

fn bind<A>(keystrokes: &str, context: &'static str) -> KeyBinding
where
    A: Action + Default + 'static,
{
    KeyBinding::new(keystrokes, A::default(), Some(context))
}

fn bindings_for_override(cx: &mut App, binding: &KeyBindingOverride) -> Vec<KeyBinding> {
    let action_name = binding.action.trim();
    let keystrokes = binding.keystrokes.trim();
    if keystrokes.is_empty() {
        return Vec::new();
    }

    let mut actions = known_actions(action_name);
    if actions.is_empty() {
        match cx.build_action(action_name, None) {
            Ok(action) => actions.push((action, None)),
            Err(error) => {
                eprintln!("failed to build action for keybinding `{action_name}`: {error}");
                return Vec::new();
            }
        }
    }

    actions
        .into_iter()
        .filter_map(|(action, context)| {
            binding_for_action(cx, action_name, keystrokes, action, context)
        })
        .collect()
}

fn binding_for_action(
    cx: &mut App,
    action_name: &str,
    keystrokes: &str,
    action: Box<dyn Action>,
    context: Option<&'static str>,
) -> Option<KeyBinding> {
    let context = match context {
        Some(context) => match KeyBindingContextPredicate::parse(context) {
            Ok(predicate) => Some(Rc::new(predicate)),
            Err(error) => {
                eprintln!("failed to parse keybinding context `{context}`: {error:#}");
                return None;
            }
        },
        None => None,
    };

    match KeyBinding::load(
        keystrokes,
        action,
        context,
        false,
        None,
        cx.keyboard_mapper().as_ref(),
    ) {
        Ok(binding) => Some(binding),
        Err(error) => {
            eprintln!("failed to parse keybinding `{action_name}` `{keystrokes}`: {error}");
            None
        }
    }
}

fn known_actions(action_name: &str) -> Vec<(Box<dyn Action>, Option<&'static str>)> {
    let action_name = action_name.trim();
    let exact: Vec<_> = action_specs()
        .iter()
        .filter_map(|spec| {
            let action = (spec.build)();
            (action.name() == action_name).then_some((action, spec.context))
        })
        .collect();
    if !exact.is_empty() {
        return exact;
    }

    let normalized = normalize_action_key(action_name);
    action_specs()
        .iter()
        .filter(|spec| {
            spec.aliases
                .iter()
                .any(|alias| normalize_action_key(alias) == normalized)
        })
        .map(|spec| ((spec.build)(), spec.context))
        .collect()
}

fn normalize_action_key(value: &str) -> String {
    let mut normalized = String::new();
    let mut previous_separator = true;

    for ch in value.trim().chars() {
        match ch {
            '-' | '_' | ' ' => {
                if !previous_separator {
                    normalized.push('-');
                    previous_separator = true;
                }
            }
            ch if ch.is_ascii_uppercase() => {
                if !previous_separator {
                    normalized.push('-');
                }
                normalized.push(ch.to_ascii_lowercase());
                previous_separator = false;
            }
            ch => {
                normalized.push(ch.to_ascii_lowercase());
                previous_separator = false;
            }
        }
    }

    normalized.trim_matches('-').to_string()
}

fn boxed<A>() -> Box<dyn Action>
where
    A: Action + Default + 'static,
{
    Box::new(A::default())
}

fn action_specs() -> &'static [ActionSpec] {
    &[
        ActionSpec {
            aliases: &["copy-selection", "copy"],
            context: Some(PDF_CONTEXT),
            build: boxed::<CopySelection>,
        },
        ActionSpec {
            aliases: &["select-all"],
            context: Some(PDF_CONTEXT),
            build: boxed::<SelectAll>,
        },
        ActionSpec {
            aliases: &["zoom-in"],
            context: Some(PDF_CONTEXT),
            build: boxed::<ZoomIn>,
        },
        ActionSpec {
            aliases: &["zoom-out"],
            context: Some(PDF_CONTEXT),
            build: boxed::<ZoomOut>,
        },
        ActionSpec {
            aliases: &["reset-zoom"],
            context: Some(PDF_CONTEXT),
            build: boxed::<ResetZoom>,
        },
        ActionSpec {
            aliases: &["reload", "reload-document"],
            context: Some(PDF_CONTEXT),
            build: boxed::<Reload>,
        },
        ActionSpec {
            aliases: &["search"],
            context: Some(PDF_CONTEXT),
            build: boxed::<Search>,
        },
        ActionSpec {
            aliases: &["search-next"],
            context: Some(PDF_CONTEXT),
            build: boxed::<SearchNext>,
        },
        ActionSpec {
            aliases: &["search-prev", "search-previous"],
            context: Some(PDF_CONTEXT),
            build: boxed::<SearchPrev>,
        },
        ActionSpec {
            aliases: &["next-page"],
            context: Some(PDF_CONTEXT),
            build: boxed::<NextPage>,
        },
        ActionSpec {
            aliases: &["previous-page", "prev-page"],
            context: Some(PDF_CONTEXT),
            build: boxed::<PreviousPage>,
        },
        ActionSpec {
            aliases: &["next-half-page"],
            context: Some(PDF_CONTEXT),
            build: boxed::<NextHalfPage>,
        },
        ActionSpec {
            aliases: &["previous-half-page", "prev-half-page"],
            context: Some(PDF_CONTEXT),
            build: boxed::<PreviousHalfPage>,
        },
        ActionSpec {
            aliases: &[
                "go-to-beginning",
                "go-to-start",
                "go-to-top",
                "go-to-the-beginning",
                "start-of-document",
            ],
            context: Some(PDF_CONTEXT),
            build: boxed::<GoToBeginning>,
        },
        ActionSpec {
            aliases: &[
                "go-to-end",
                "go-to-bottom",
                "go-to-the-end",
                "end-of-document",
            ],
            context: Some(PDF_CONTEXT),
            build: boxed::<GoToEnd>,
        },
        ActionSpec {
            aliases: &["jump-backward", "jump-back"],
            context: Some(PDF_CONTEXT),
            build: boxed::<JumpBackward>,
        },
        ActionSpec {
            aliases: &["jump-forward"],
            context: Some(PDF_CONTEXT),
            build: boxed::<JumpForward>,
        },
        ActionSpec {
            aliases: &["toc", "goto-toc", "goto-table-of-contents"],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<Toc>,
        },
        ActionSpec {
            aliases: &[
                "toggle-toc",
                "toggle-table-of-contents",
                "table-of-contents",
            ],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<ToggleToc>,
        },
        ActionSpec {
            aliases: &["open-notes", "notes"],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<OpenNotes>,
        },
        ActionSpec {
            aliases: &["toggle-notes", "toggle-notes-sidebar"],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<ToggleNotes>,
        },
        ActionSpec {
            aliases: &[
                "toggle-thumbnails",
                "toggle-thumbnail-sidebar",
                "thumbnail-sidebar",
                "page-thumbnails",
            ],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<ToggleThumbnailSidebar>,
        },
        ActionSpec {
            aliases: &["toggle-grid-overview", "grid-overview", "page-overview"],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<ToggleGridOverview>,
        },
        ActionSpec {
            aliases: &[
                "grid-overview-confirm",
                "grid-overview-open",
                "open-grid-overview-page",
            ],
            context: Some(GRID_OVERVIEW_CONTEXT),
            build: boxed::<GridOverviewConfirm>,
        },
        ActionSpec {
            aliases: &["grid-cursor-up", "grid-overview-cursor-up"],
            context: Some(GRID_OVERVIEW_CONTEXT),
            build: boxed::<GridCursorUp>,
        },
        ActionSpec {
            aliases: &["grid-cursor-down", "grid-overview-cursor-down"],
            context: Some(GRID_OVERVIEW_CONTEXT),
            build: boxed::<GridCursorDown>,
        },
        ActionSpec {
            aliases: &["grid-cursor-left", "grid-overview-cursor-left"],
            context: Some(GRID_OVERVIEW_CONTEXT),
            build: boxed::<GridCursorLeft>,
        },
        ActionSpec {
            aliases: &["grid-cursor-right", "grid-overview-cursor-right"],
            context: Some(GRID_OVERVIEW_CONTEXT),
            build: boxed::<GridCursorRight>,
        },
        ActionSpec {
            aliases: &["toggle-command-palette", "command-palette"],
            context: Some(COMMAND_PALETTE_CONTEXT),
            build: boxed::<ToggleCommandPalette>,
        },
        ActionSpec {
            aliases: &["toggle-full-screen", "toggle-fullscreen", "fullscreen"],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<ToggleFullScreen>,
        },
        ActionSpec {
            aliases: &["toggle-transparency", "transparency"],
            context: Some(PDF_CONTEXT),
            build: boxed::<ToggleTransparency>,
        },
        ActionSpec {
            aliases: &["open-settings", "settings"],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<OpenSettings>,
        },
        ActionSpec {
            aliases: &["toggle-ai-panel", "ai-panel", "ai", "assistant"],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<ToggleAiPanel>,
        },
        ActionSpec {
            aliases: &["open-history", "history", "recent-files"],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<OpenHistory>,
        },
        ActionSpec {
            aliases: &["open-bookmarks", "bookmarks"],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<OpenBookmarks>,
        },
        ActionSpec {
            aliases: &["open-file", "open-pdf"],
            context: Some(WORKSPACE_CONTEXT),
            build: boxed::<OpenFile>,
        },
        ActionSpec {
            aliases: &["open-new-tab", "new-tab"],
            context: Some(WORKSPACE_CONTEXT),
            build: boxed::<OpenNewTab>,
        },
        ActionSpec {
            aliases: &["close-tab"],
            context: Some(WORKSPACE_CONTEXT),
            build: boxed::<CloseTab>,
        },
        ActionSpec {
            aliases: &["add-bookmark", "bookmark-page"],
            context: Some(PDF_CONTEXT),
            build: boxed::<AddBookmark>,
        },
        ActionSpec {
            aliases: &["note-save", "save-note"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteSave>,
        },
        ActionSpec {
            aliases: &["note-cancel", "cancel-note"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteCancel>,
        },
        ActionSpec {
            aliases: &["note-newline"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteNewline>,
        },
        ActionSpec {
            aliases: &["note-backspace"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteBackspace>,
        },
        ActionSpec {
            aliases: &["note-delete"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteDelete>,
        },
        ActionSpec {
            aliases: &["note-left"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteLeft>,
        },
        ActionSpec {
            aliases: &["note-right"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteRight>,
        },
        ActionSpec {
            aliases: &["note-up"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteUp>,
        },
        ActionSpec {
            aliases: &["note-down"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteDown>,
        },
        ActionSpec {
            aliases: &["note-home"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteHome>,
        },
        ActionSpec {
            aliases: &["note-end"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteEnd>,
        },
        ActionSpec {
            aliases: &["note-select-all"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteSelectAll>,
        },
        ActionSpec {
            aliases: &["note-select-left"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteSelectLeft>,
        },
        ActionSpec {
            aliases: &["note-select-right"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteSelectRight>,
        },
        ActionSpec {
            aliases: &["note-select-up"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteSelectUp>,
        },
        ActionSpec {
            aliases: &["note-select-down"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteSelectDown>,
        },
        ActionSpec {
            aliases: &["note-select-home"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteSelectHome>,
        },
        ActionSpec {
            aliases: &["note-select-end"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteSelectEnd>,
        },
        ActionSpec {
            aliases: &["note-copy", "copy"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteCopy>,
        },
        ActionSpec {
            aliases: &["note-cut", "cut"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NoteCut>,
        },
        ActionSpec {
            aliases: &["note-paste"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NotePaste>,
        },
        ActionSpec {
            aliases: &["ai-panel-submit", "ai-submit", "send-ai-message"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelSubmit>,
        },
        ActionSpec {
            aliases: &["ai-panel-close", "close-ai-panel"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelClose>,
        },
        ActionSpec {
            aliases: &["ai-panel-newline", "ai-newline"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelNewline>,
        },
        ActionSpec {
            aliases: &["ai-panel-backspace", "ai-backspace"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelBackspace>,
        },
        ActionSpec {
            aliases: &["ai-panel-delete", "ai-delete"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelDelete>,
        },
        ActionSpec {
            aliases: &["ai-panel-left", "ai-left"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelLeft>,
        },
        ActionSpec {
            aliases: &["ai-panel-right", "ai-right"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelRight>,
        },
        ActionSpec {
            aliases: &["ai-panel-up", "ai-up"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelUp>,
        },
        ActionSpec {
            aliases: &["ai-panel-down", "ai-down"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelDown>,
        },
        ActionSpec {
            aliases: &["ai-panel-home", "ai-home"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelHome>,
        },
        ActionSpec {
            aliases: &["ai-panel-end", "ai-end"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelEnd>,
        },
        ActionSpec {
            aliases: &["ai-panel-select-all", "ai-select-all"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelSelectAll>,
        },
        ActionSpec {
            aliases: &["ai-panel-select-left", "ai-select-left"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelSelectLeft>,
        },
        ActionSpec {
            aliases: &["ai-panel-select-right", "ai-select-right"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelSelectRight>,
        },
        ActionSpec {
            aliases: &["ai-panel-select-up", "ai-select-up"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelSelectUp>,
        },
        ActionSpec {
            aliases: &["ai-panel-select-down", "ai-select-down"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelSelectDown>,
        },
        ActionSpec {
            aliases: &["ai-panel-select-home", "ai-select-home"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelSelectHome>,
        },
        ActionSpec {
            aliases: &["ai-panel-select-end", "ai-select-end"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelSelectEnd>,
        },
        ActionSpec {
            aliases: &["ai-panel-copy", "ai-copy", "copy"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelCopy>,
        },
        ActionSpec {
            aliases: &["ai-panel-cut", "ai-cut", "cut"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelCut>,
        },
        ActionSpec {
            aliases: &["ai-panel-paste", "ai-paste"],
            context: Some(AI_PANEL_CONTEXT),
            build: boxed::<AiPanelPaste>,
        },
        ActionSpec {
            aliases: &["palette-confirm"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerConfirm>,
        },
        ActionSpec {
            aliases: &["palette-cancel"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerCancel>,
        },
        ActionSpec {
            aliases: &["palette-select-next"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerSelectNext>,
        },
        ActionSpec {
            aliases: &["palette-select-prev", "palette-select-previous"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerSelectPrev>,
        },
        ActionSpec {
            aliases: &["palette-backspace"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerBackspace>,
        },
        ActionSpec {
            aliases: &["palette-delete"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerDelete>,
        },
        ActionSpec {
            aliases: &["palette-left"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerLeft>,
        },
        ActionSpec {
            aliases: &["palette-right"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerRight>,
        },
        ActionSpec {
            aliases: &["palette-home"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerHome>,
        },
        ActionSpec {
            aliases: &["palette-end"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerEnd>,
        },
        ActionSpec {
            aliases: &["palette-select-all"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerSelectAll>,
        },
        ActionSpec {
            aliases: &["palette-select-left"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerSelectLeft>,
        },
        ActionSpec {
            aliases: &["palette-select-right"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerSelectRight>,
        },
        ActionSpec {
            aliases: &["palette-select-home"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerSelectHome>,
        },
        ActionSpec {
            aliases: &["palette-select-end"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerSelectEnd>,
        },
        ActionSpec {
            aliases: &["palette-copy", "copy"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerCopy>,
        },
        ActionSpec {
            aliases: &["palette-cut", "cut"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerCut>,
        },
        ActionSpec {
            aliases: &["palette-paste"],
            context: Some(PALETTE_CONTEXT),
            build: boxed::<PickerPaste>,
        },
        ActionSpec {
            aliases: &["search-input-confirm"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputConfirm>,
        },
        ActionSpec {
            aliases: &["search-input-cancel"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputCancel>,
        },
        ActionSpec {
            aliases: &["search-input-previous", "search-input-prev"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputPrevious>,
        },
        ActionSpec {
            aliases: &["search-input-backspace"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputBackspace>,
        },
        ActionSpec {
            aliases: &["search-input-delete"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputDelete>,
        },
        ActionSpec {
            aliases: &["search-input-left"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputLeft>,
        },
        ActionSpec {
            aliases: &["search-input-right"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputRight>,
        },
        ActionSpec {
            aliases: &["search-input-home"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputHome>,
        },
        ActionSpec {
            aliases: &["search-input-end"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputEnd>,
        },
        ActionSpec {
            aliases: &["search-input-select-all"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputSelectAll>,
        },
        ActionSpec {
            aliases: &["search-input-select-left"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputSelectLeft>,
        },
        ActionSpec {
            aliases: &["search-input-select-right"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputSelectRight>,
        },
        ActionSpec {
            aliases: &["search-input-select-home"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputSelectHome>,
        },
        ActionSpec {
            aliases: &["search-input-select-end"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputSelectEnd>,
        },
        ActionSpec {
            aliases: &["search-input-copy", "copy"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputCopy>,
        },
        ActionSpec {
            aliases: &["search-input-cut", "cut"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputCut>,
        },
        ActionSpec {
            aliases: &["search-input-paste"],
            context: Some(SEARCH_CONTEXT),
            build: boxed::<SearchInputPaste>,
        },
        ActionSpec {
            aliases: &["toc-confirm"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerConfirm>,
        },
        ActionSpec {
            aliases: &["toc-cancel"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerCancel>,
        },
        ActionSpec {
            aliases: &["toc-select-next"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerSelectNext>,
        },
        ActionSpec {
            aliases: &["toc-select-prev", "toc-select-previous"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerSelectPrev>,
        },
        ActionSpec {
            aliases: &["toc-backspace"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerBackspace>,
        },
        ActionSpec {
            aliases: &["toc-delete"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerDelete>,
        },
        ActionSpec {
            aliases: &["toc-left"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerLeft>,
        },
        ActionSpec {
            aliases: &["toc-right"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerRight>,
        },
        ActionSpec {
            aliases: &["toc-home"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerHome>,
        },
        ActionSpec {
            aliases: &["toc-end"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerEnd>,
        },
        ActionSpec {
            aliases: &["toc-select-all"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerSelectAll>,
        },
        ActionSpec {
            aliases: &["toc-select-left"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerSelectLeft>,
        },
        ActionSpec {
            aliases: &["toc-select-right"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerSelectRight>,
        },
        ActionSpec {
            aliases: &["toc-select-home"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerSelectHome>,
        },
        ActionSpec {
            aliases: &["toc-select-end"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerSelectEnd>,
        },
        ActionSpec {
            aliases: &["toc-copy", "copy"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerCopy>,
        },
        ActionSpec {
            aliases: &["toc-cut", "cut"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerCut>,
        },
        ActionSpec {
            aliases: &["toc-paste"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerPaste>,
        },
        ActionSpec {
            aliases: &["notes-select-left"],
            context: Some(NOTES_CONTEXT),
            build: boxed::<PickerSelectLeft>,
        },
        ActionSpec {
            aliases: &["notes-select-right"],
            context: Some(NOTES_CONTEXT),
            build: boxed::<PickerSelectRight>,
        },
        ActionSpec {
            aliases: &["notes-select-home"],
            context: Some(NOTES_CONTEXT),
            build: boxed::<PickerSelectHome>,
        },
        ActionSpec {
            aliases: &["notes-select-end"],
            context: Some(NOTES_CONTEXT),
            build: boxed::<PickerSelectEnd>,
        },
        ActionSpec {
            aliases: &["notes-copy", "copy"],
            context: Some(NOTES_CONTEXT),
            build: boxed::<PickerCopy>,
        },
        ActionSpec {
            aliases: &["notes-cut", "cut"],
            context: Some(NOTES_CONTEXT),
            build: boxed::<PickerCut>,
        },
        ActionSpec {
            aliases: &["history-select-left"],
            context: Some(HISTORY_CONTEXT),
            build: boxed::<PickerSelectLeft>,
        },
        ActionSpec {
            aliases: &["history-select-right"],
            context: Some(HISTORY_CONTEXT),
            build: boxed::<PickerSelectRight>,
        },
        ActionSpec {
            aliases: &["history-select-home"],
            context: Some(HISTORY_CONTEXT),
            build: boxed::<PickerSelectHome>,
        },
        ActionSpec {
            aliases: &["history-select-end"],
            context: Some(HISTORY_CONTEXT),
            build: boxed::<PickerSelectEnd>,
        },
        ActionSpec {
            aliases: &["history-copy", "copy"],
            context: Some(HISTORY_CONTEXT),
            build: boxed::<PickerCopy>,
        },
        ActionSpec {
            aliases: &["history-cut", "cut"],
            context: Some(HISTORY_CONTEXT),
            build: boxed::<PickerCut>,
        },
        ActionSpec {
            aliases: &["bookmarks-select-left"],
            context: Some(BOOKMARKS_CONTEXT),
            build: boxed::<PickerSelectLeft>,
        },
        ActionSpec {
            aliases: &["bookmarks-select-right"],
            context: Some(BOOKMARKS_CONTEXT),
            build: boxed::<PickerSelectRight>,
        },
        ActionSpec {
            aliases: &["bookmarks-select-home"],
            context: Some(BOOKMARKS_CONTEXT),
            build: boxed::<PickerSelectHome>,
        },
        ActionSpec {
            aliases: &["bookmarks-select-end"],
            context: Some(BOOKMARKS_CONTEXT),
            build: boxed::<PickerSelectEnd>,
        },
        ActionSpec {
            aliases: &["bookmarks-copy", "copy"],
            context: Some(BOOKMARKS_CONTEXT),
            build: boxed::<PickerCopy>,
        },
        ActionSpec {
            aliases: &["bookmarks-cut", "cut"],
            context: Some(BOOKMARKS_CONTEXT),
            build: boxed::<PickerCut>,
        },
        ActionSpec {
            aliases: &["bookmark-prompt-select-left"],
            context: Some(BOOKMARK_PROMPT_CONTEXT),
            build: boxed::<PickerSelectLeft>,
        },
        ActionSpec {
            aliases: &["bookmark-prompt-select-right"],
            context: Some(BOOKMARK_PROMPT_CONTEXT),
            build: boxed::<PickerSelectRight>,
        },
        ActionSpec {
            aliases: &["bookmark-prompt-select-home"],
            context: Some(BOOKMARK_PROMPT_CONTEXT),
            build: boxed::<PickerSelectHome>,
        },
        ActionSpec {
            aliases: &["bookmark-prompt-select-end"],
            context: Some(BOOKMARK_PROMPT_CONTEXT),
            build: boxed::<PickerSelectEnd>,
        },
        ActionSpec {
            aliases: &["bookmark-prompt-copy", "copy"],
            context: Some(BOOKMARK_PROMPT_CONTEXT),
            build: boxed::<PickerCopy>,
        },
        ActionSpec {
            aliases: &["bookmark-prompt-cut", "cut"],
            context: Some(BOOKMARK_PROMPT_CONTEXT),
            build: boxed::<PickerCut>,
        },
        ActionSpec {
            aliases: &["settings-input-select-left"],
            context: Some(SETTINGS_INPUT_CONTEXT),
            build: boxed::<SettingsInputSelectLeft>,
        },
        ActionSpec {
            aliases: &["settings-input-select-right"],
            context: Some(SETTINGS_INPUT_CONTEXT),
            build: boxed::<SettingsInputSelectRight>,
        },
        ActionSpec {
            aliases: &["settings-input-select-home"],
            context: Some(SETTINGS_INPUT_CONTEXT),
            build: boxed::<SettingsInputSelectHome>,
        },
        ActionSpec {
            aliases: &["settings-input-select-end"],
            context: Some(SETTINGS_INPUT_CONTEXT),
            build: boxed::<SettingsInputSelectEnd>,
        },
        ActionSpec {
            aliases: &["settings-input-copy", "copy"],
            context: Some(SETTINGS_INPUT_CONTEXT),
            build: boxed::<SettingsInputCopy>,
        },
        ActionSpec {
            aliases: &["settings-input-cut", "cut"],
            context: Some(SETTINGS_INPUT_CONTEXT),
            build: boxed::<SettingsInputCut>,
        },
        ActionSpec {
            aliases: &["panel-close", "settings-panel-close"],
            context: Some(SETTINGS_CONTEXT),
            build: boxed::<PanelClose>,
        },
    ]
}
