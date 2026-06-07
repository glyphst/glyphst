use std::rc::Rc;

use crate::{
    pdf_view::{
        CopySelection, JumpBackward, JumpForward, NextHalfPage, NextPage, OpenSettings,
        PreviousHalfPage, PreviousPage, ResetZoom, Search, SearchNext, SearchPrev, SelectAll, Toc,
        ToggleCommandPalette, ToggleFullScreen, ToggleToc, ToggleTransparency, ZoomIn, ZoomOut,
    },
    picker::{
        PickerBackspace, PickerCancel, PickerConfirm, PickerDelete, PickerEnd, PickerHome,
        PickerLeft, PickerPaste, PickerRight, PickerSelectAll, PickerSelectNext, PickerSelectPrev,
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

const PDF_CONTEXT: &str = "PdfView && !CommandPalette && !Search && !Toc";
const PDF_NO_SEARCH_CONTEXT: &str = "PdfView && !Search && !Toc";
const PALETTE_CONTEXT: &str = "CommandPalette";
const SEARCH_CONTEXT: &str = "Search";
const SETTINGS_CONTEXT: &str = "SettingsPanel";
const TOC_CONTEXT: &str = "Toc";

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
    ]);

    match preset {
        KeymapPreset::Modern => bindings.extend([
            bind::<Search>("ctrl-f", PDF_CONTEXT),
            bind::<Search>("cmd-f", PDF_CONTEXT),
            bind::<Search>("/", PDF_CONTEXT),
            bind::<SearchNext>("f3", PDF_CONTEXT),
            bind::<SearchPrev>("shift-f3", PDF_CONTEXT),
            bind::<NextPage>("pagedown", PDF_CONTEXT),
            bind::<PreviousPage>("pageup", PDF_CONTEXT),
            bind::<NextHalfPage>("shift-pagedown", PDF_CONTEXT),
            bind::<PreviousHalfPage>("shift-pageup", PDF_CONTEXT),
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
