use std::rc::Rc;

use crate::{
    note_editor::{
        NoteBackspace, NoteCancel, NoteDelete, NoteDown, NoteEnd, NoteHome, NoteLeft, NoteNewline,
        NotePaste, NoteRight, NoteSave, NoteSelectAll, NoteUp,
    },
    pdf_view::{
        AddBookmark, CopySelection, JumpBackward, JumpForward, NextHalfPage, NextPage,
        OpenBookmarks, OpenFile, OpenHistory, OpenNotes, OpenSettings, PreviousHalfPage,
        PreviousPage, ResetZoom, Search, SearchNext, SearchPrev, SelectAll, Toc,
        ToggleCommandPalette, ToggleFullScreen, ToggleGridOverview, ToggleNotes, ToggleToc,
        ToggleTransparency, ZoomIn, ZoomOut,
    },
    picker::{
        PickerBackspace, PickerCancel, PickerConfirm, PickerDelete, PickerEnd, PickerHome,
        PickerLeft, PickerPaste, PickerRemove, PickerRight, PickerSelectAll, PickerSelectNext,
        PickerSelectPrev,
    },
    search::{
        SearchInputBackspace, SearchInputCancel, SearchInputConfirm, SearchInputDelete,
        SearchInputEnd, SearchInputHome, SearchInputLeft, SearchInputPaste, SearchInputPrevious,
        SearchInputRight, SearchInputSelectAll,
    },
    settings::{KeyBindingOverride, KeymapPreset},
    settings_panel::PanelClose,
};
use gpui::{Action, App, KeyBinding, KeyBindingContextPredicate};

const PDF_CONTEXT: &str = "PdfView && !GridOverview && !CommandPalette && !Search && !Toc && !Notes && !History && !Bookmarks && !BookmarkPrompt && !NoteEditor";
const PDF_NO_SEARCH_CONTEXT: &str = "PdfView && !GridOverview && !Search && !Toc && !Notes && !History && !Bookmarks && !BookmarkPrompt && !NoteEditor";
const GRID_OVERVIEW_CONTEXT: &str = "GridOverview";
const PALETTE_CONTEXT: &str = "CommandPalette";
const SEARCH_CONTEXT: &str = "Search";
const SETTINGS_CONTEXT: &str = "SettingsPanel";
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
    bindings.extend(
        overrides
            .iter()
            .filter_map(|binding| binding_for_override(cx, binding)),
    );

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
        bind::<ToggleCommandPalette>("ctrl-shift-p", PDF_NO_SEARCH_CONTEXT),
        bind::<ToggleCommandPalette>("cmd-shift-p", PDF_NO_SEARCH_CONTEXT),
        bind::<ToggleFullScreen>("f11", PDF_NO_SEARCH_CONTEXT),
        bind::<ToggleTransparency>("ctrl-shift-t", PDF_CONTEXT),
        bind::<ToggleTransparency>("cmd-shift-t", PDF_CONTEXT),
        bind::<OpenSettings>("ctrl-,", PDF_NO_SEARCH_CONTEXT),
        bind::<OpenSettings>("cmd-,", PDF_NO_SEARCH_CONTEXT),
        bind::<OpenHistory>("ctrl-h", PDF_NO_SEARCH_CONTEXT),
        bind::<OpenFile>("cmd-o", PDF_NO_SEARCH_CONTEXT),
        bind::<ToggleGridOverview>("ctrl-g", PDF_NO_SEARCH_CONTEXT),
        bind::<ZoomIn>("+", GRID_OVERVIEW_CONTEXT),
        bind::<ZoomIn>("shift-=", GRID_OVERVIEW_CONTEXT),
        bind::<ZoomOut>("-", GRID_OVERVIEW_CONTEXT),
        bind::<ToggleGridOverview>("ctrl-g", GRID_OVERVIEW_CONTEXT),
        bind::<ToggleGridOverview>("escape", GRID_OVERVIEW_CONTEXT),
    ]);

    match preset {
        KeymapPreset::Modern => bindings.extend([
            bind::<OpenFile>("ctrl-o", PDF_NO_SEARCH_CONTEXT),
            bind::<Search>("ctrl-f", PDF_CONTEXT),
            bind::<Search>("cmd-f", PDF_CONTEXT),
            bind::<Search>("/", PDF_CONTEXT),
            bind::<SearchNext>("f3", PDF_CONTEXT),
            bind::<SearchPrev>("shift-f3", PDF_CONTEXT),
            bind::<NextPage>("pagedown", PDF_CONTEXT),
            bind::<PreviousPage>("pageup", PDF_CONTEXT),
            bind::<NextHalfPage>("shift-pagedown", PDF_CONTEXT),
            bind::<PreviousHalfPage>("shift-pageup", PDF_CONTEXT),
            bind::<OpenBookmarks>("ctrl-b", PDF_NO_SEARCH_CONTEXT),
            bind::<AddBookmark>("ctrl-d", PDF_CONTEXT),
        ]),
        KeymapPreset::Vim => bindings.extend([
            bind::<Search>("/", PDF_CONTEXT),
            bind::<SearchNext>("n", PDF_CONTEXT),
            bind::<SearchPrev>("shift-n", PDF_CONTEXT),
            bind::<NextPage>("ctrl-f", PDF_CONTEXT),
            bind::<PreviousPage>("ctrl-b", PDF_CONTEXT),
            bind::<NextHalfPage>("ctrl-d", PDF_CONTEXT),
            bind::<PreviousHalfPage>("ctrl-u", PDF_CONTEXT),
            bind::<JumpBackward>("ctrl-o", PDF_CONTEXT),
            bind::<JumpForward>("ctrl-i", PDF_CONTEXT),
            bind::<OpenBookmarks>("'", PDF_NO_SEARCH_CONTEXT),
            bind::<AddBookmark>("m", PDF_CONTEXT),
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
        bind::<SearchInputPaste>("ctrl-v", SEARCH_CONTEXT),
        bind::<SearchInputPaste>("cmd-v", SEARCH_CONTEXT),
        bind::<PanelClose>("escape", SETTINGS_CONTEXT),
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
        bind::<NotePaste>("ctrl-v", NOTE_EDITOR_CONTEXT),
        bind::<NotePaste>("cmd-v", NOTE_EDITOR_CONTEXT),
    ]);

    bindings
}

fn bind<A>(keystrokes: &str, context: &'static str) -> KeyBinding
where
    A: Action + Default + 'static,
{
    KeyBinding::new(keystrokes, A::default(), Some(context))
}

fn binding_for_override(cx: &mut App, binding: &KeyBindingOverride) -> Option<KeyBinding> {
    let action_name = binding.action.trim();
    let keystrokes = binding.keystrokes.trim();
    if keystrokes.is_empty() {
        return None;
    }

    let (action, context) = match known_action(action_name) {
        Some(action) => action,
        None => match cx.build_action(action_name, None) {
            Ok(action) => (action, None),
            Err(error) => {
                eprintln!("failed to build action for keybinding `{action_name}`: {error}");
                return None;
            }
        },
    };

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

fn known_action(action_name: &str) -> Option<(Box<dyn Action>, Option<&'static str>)> {
    let action_name = action_name.trim();
    for spec in action_specs() {
        let action = (spec.build)();
        if action.name() == action_name {
            return Some((action, spec.context));
        }
    }

    let normalized = normalize_action_key(action_name);
    action_specs().iter().find_map(|spec| {
        spec.aliases
            .iter()
            .any(|alias| normalize_action_key(alias) == normalized)
            .then(|| ((spec.build)(), spec.context))
    })
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
            aliases: &["toggle-grid-overview", "grid-overview", "page-overview"],
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<ToggleGridOverview>,
        },
        ActionSpec {
            aliases: &["toggle-command-palette", "command-palette"],
            context: Some(PDF_NO_SEARCH_CONTEXT),
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
            context: Some(PDF_NO_SEARCH_CONTEXT),
            build: boxed::<OpenFile>,
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
            aliases: &["note-paste"],
            context: Some(NOTE_EDITOR_CONTEXT),
            build: boxed::<NotePaste>,
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
            aliases: &["toc-paste"],
            context: Some(TOC_CONTEXT),
            build: boxed::<PickerPaste>,
        },
        ActionSpec {
            aliases: &["panel-close", "settings-panel-close"],
            context: Some(SETTINGS_CONTEXT),
            build: boxed::<PanelClose>,
        },
    ]
}
