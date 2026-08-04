use std::{rc::Rc, sync::OnceLock};

use glyphst_actions::{
    ai_panel::AiPanelClose,
    document_viewer::{
        AddBookmark, CancelDocumentTranslation, CancelRegionSelection, CloseTab, CopySelection,
        DuplicateTab, GoToBeginning, GoToEnd, GridCursorDown, GridCursorLeft, GridCursorRight,
        GridCursorUp, GridOverviewConfirm, JumpBackward, JumpForward, NextHalfPage, NextPage,
        NextSearchResult, OpenBookmarks, OpenFile, OpenGoToPage, OpenHistory, OpenKeybindings,
        OpenNewTab, OpenNotes, OpenQuickAsk, OpenSettings, PrevSearchResult, PreviousHalfPage,
        PreviousPage, Redo, Reload, ResetZoom, RotateClockwise, RotateCounterclockwise,
        ScrollLineDown, ScrollLineUp, Search, SelectAll, Toc, ToggleAiPanel, ToggleCommandPalette,
        ToggleFullScreen, ToggleGridOverview, ToggleNotes, TogglePinnedTranslation,
        TogglePresentationMode, TogglePublisherStyles, ToggleRegionSelection,
        ToggleThumbnailSidebar, ToggleToc, ToggleTransparency, TranslateDocument,
        TranslateSelection, Undo, ZoomIn, ZoomOut,
    },
    menu,
    new_tab::{
        ActivateFocused, CancelSelect, CursorDown, CursorFirst, CursorLast, CursorLeft,
        CursorRight, CursorUp, DeleteSelected, EnterSelect, ExtendSelectionDown,
        ExtendSelectionFirst, ExtendSelectionLast, ExtendSelectionLeft, ExtendSelectionRight,
        ExtendSelectionUp, SelectAll as NewTabSelectAll, ToggleFocused,
    },
    note_editor::NoteCancel,
    pane::{SplitDown, SplitHorizontal, SplitLeft, SplitRight, SplitUp, SplitVertical},
    search::{SearchInputCancel, SearchInputConfirm, SearchInputPrevious},
    settings_panel::PanelClose,
    workspace::{
        ActivateNextPane, ActivatePaneDown, ActivatePaneLeft, ActivatePaneRight, ActivatePaneUp,
        ActivatePreviousPane, CloseOtherPanes, MovePaneDown, MovePaneLeft, MovePaneRight,
        MovePaneUp, ResetPaneSizes, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp,
        SwapPaneDown, SwapPaneLeft, SwapPaneRight, SwapPaneUp, ToggleMode, ToggleThemeSelector,
    },
};
use glyphst_settings::{KeyBindingOverride, KeymapPreset, Settings};
use gpui::{Action, App, Global, KeyBinding, KeyBindingContextPredicate};

struct FrameworkKeyBindings(Vec<KeyBinding>);

impl Global for FrameworkKeyBindings {}

/// Capture bindings registered by the UI framework before Glyphst applies its
/// configurable keymap.
///
/// Glyphst rebuilds its bindings when the preset or overrides change. Keeping
/// this baseline prevents that rebuild from erasing gpui-component's internal
/// input, focus, menu, and dialog bindings.
pub fn capture_framework_bindings(cx: &mut App) {
    let bindings = cx.key_bindings().borrow().bindings().cloned().collect();
    cx.set_global(FrameworkKeyBindings(bindings));
}

/// Install platform input bindings that Glyphst expects but gpui-component
/// intentionally does not define outside macOS.
pub fn install_input_compatibility_bindings(cx: &mut App) {
    #[cfg(not(target_os = "macos"))]
    {
        use gpui_component::input::{
            Copy, Cut, MoveToEnd, MoveToStart, Paste, SelectToEnd, SelectToStart,
        };

        const INPUT_CONTEXT: &str = "Input";
        cx.bind_keys([
            KeyBinding::new("shift-delete", Cut, Some(INPUT_CONTEXT)),
            KeyBinding::new("ctrl-home", MoveToStart, Some(INPUT_CONTEXT)),
            KeyBinding::new("ctrl-end", MoveToEnd, Some(INPUT_CONTEXT)),
            KeyBinding::new("ctrl-shift-home", SelectToStart, Some(INPUT_CONTEXT)),
            KeyBinding::new("ctrl-shift-end", SelectToEnd, Some(INPUT_CONTEXT)),
            KeyBinding::new("ctrl-insert", Copy, Some(INPUT_CONTEXT)),
            KeyBinding::new("shift-insert", Paste, Some(INPUT_CONTEXT)),
        ]);
    }
}

const OVERLAY_CONTEXTS: &[&str] = &[
    "GridOverview",
    "CommandPalette",
    "Search",
    "QuickAsk",
    "DocumentBrowser",
    "Toc",
    "Notes",
    "History",
    "Bookmarks",
    "BookmarkPrompt",
    "GoToPage",
    "NoteEditor",
    "AiPanel",
    "SettingsPanel",
    "ThemeSelector",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyBindingCategory {
    Workspace,
    DocumentView,
    PageOverview,
    NewTab,
    SearchResults,
}

impl KeyBindingCategory {
    pub fn id(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::DocumentView => "document_view",
            Self::PageOverview => "page_overview",
            Self::NewTab => "new_tab",
            Self::SearchResults => "search_results",
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Workspace => glyphst_settings::localize::tr("keybinding-category-workspace"),
            Self::DocumentView => {
                glyphst_settings::localize::tr("keybinding-category-document-view")
            }
            Self::PageOverview => {
                glyphst_settings::localize::tr("keybinding-category-page-overview")
            }
            Self::NewTab => glyphst_settings::localize::tr("keybinding-category-new-tab"),
            Self::SearchResults => {
                glyphst_settings::localize::tr("keybinding-category-search-results")
            }
        }
    }

    pub fn description(self) -> String {
        match self {
            Self::Workspace => {
                glyphst_settings::localize::tr("keybinding-category-workspace-description")
            }
            Self::DocumentView => {
                glyphst_settings::localize::tr("keybinding-category-document-view-description")
            }
            Self::PageOverview => {
                glyphst_settings::localize::tr("keybinding-category-page-overview-description")
            }
            Self::NewTab => {
                glyphst_settings::localize::tr("keybinding-category-new-tab-description")
            }
            Self::SearchResults => {
                glyphst_settings::localize::tr("keybinding-category-search-results-description")
            }
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        match value {
            "workspace" | "global" => Some(Self::Workspace),
            "document_view" => Some(Self::DocumentView),
            "page_overview" | "grid_overview" | "overview" => Some(Self::PageOverview),
            "new_tab" | "newtab" => Some(Self::NewTab),
            "search_results" | "search" => Some(Self::SearchResults),
            _ => None,
        }
    }

    fn predicate(self) -> String {
        match self {
            Self::Workspace => context_without_overlays("Workspace"),
            Self::DocumentView => context_without_overlays("DocumentView"),
            Self::PageOverview => context_without_overlays("GridOverview"),
            Self::NewTab => context_without_overlays("NewTab"),
            Self::SearchResults => "Search".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct EditableKeyBinding {
    pub action: &'static str,
    pub label: String,
    pub category: KeyBindingCategory,
    pub keystrokes: Vec<String>,
    pub default_keystrokes: Vec<String>,
}

struct CommandSpec {
    action: &'static str,
    label: &'static str,
    aliases: &'static [&'static str],
    category: KeyBindingCategory,
    modern: &'static [&'static str],
    vim: &'static [&'static str],
    legacy_default: bool,
    build: fn() -> Box<dyn Action>,
}

pub fn apply(cx: &mut App, preset: KeymapPreset, overrides: &[KeyBindingOverride]) {
    let mut bindings = cx
        .try_global::<FrameworkKeyBindings>()
        .map(|bindings| bindings.0.clone())
        .unwrap_or_default();

    for spec in command_specs() {
        let context = spec.category.predicate();
        for keystrokes in effective_keystrokes(spec, preset, overrides) {
            add_boxed_binding(
                cx,
                &mut bindings,
                spec.action,
                &keystrokes,
                (spec.build)(),
                Some(&context),
            );
        }
    }

    add_internal_bindings(cx, &mut bindings);

    for binding in overrides {
        if binding.keymap != preset {
            continue;
        }
        if command_specs()
            .iter()
            .any(|spec| override_applies_to_spec(binding, spec))
        {
            continue;
        }
        bindings.extend(bindings_for_unknown_override(cx, binding));
    }

    cx.clear_key_bindings();
    cx.bind_keys(bindings);
}

pub fn editable_bindings(
    preset: KeymapPreset,
    overrides: &[KeyBindingOverride],
) -> Vec<EditableKeyBinding> {
    command_specs()
        .iter()
        .map(|spec| EditableKeyBinding {
            action: spec.action,
            label: localized_spec_label(spec),
            category: spec.category,
            keystrokes: effective_keystrokes(spec, preset, overrides),
            default_keystrokes: default_keystrokes(spec, preset),
        })
        .collect()
}

pub fn localized_action_label(action_name: &str) -> Option<String> {
    command_specs()
        .iter()
        .find(|spec| action_matches_spec(action_name, spec))
        .map(localized_spec_label)
}

fn localized_spec_label(spec: &CommandSpec) -> String {
    let label = glyphst_settings::localize::tr(&format!(
        "keybinding-command-{}-{}",
        spec.category.id().replace('_', "-"),
        spec.action
    ));
    if label.starts_with("No localization for id:") {
        spec.label.to_string()
    } else {
        label
    }
}

pub fn editable_categories() -> &'static [KeyBindingCategory] {
    &[
        KeyBindingCategory::Workspace,
        KeyBindingCategory::DocumentView,
        KeyBindingCategory::PageOverview,
        KeyBindingCategory::NewTab,
        KeyBindingCategory::SearchResults,
    ]
}

pub fn normalize_action_key(value: &str) -> String {
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
            ':' => {
                if !previous_separator {
                    normalized.push('-');
                    previous_separator = true;
                }
            }
            ch => {
                normalized.push(ch.to_ascii_lowercase());
                previous_separator = false;
            }
        }
    }

    normalized.trim_matches('-').to_string()
}

pub fn expand_platform_keystroke(keystrokes: &str) -> String {
    keystrokes
        .split_whitespace()
        .map(|chord| {
            chord
                .split('-')
                .map(|part| {
                    if part.eq_ignore_ascii_case("primary") {
                        platform_primary_modifier()
                    } else {
                        part
                    }
                })
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn remove_keybinding_overrides(
    overrides: &mut Vec<KeyBindingOverride>,
    preset: KeymapPreset,
    action: &str,
    category: KeyBindingCategory,
) {
    let normalized = normalize_action_key(action);
    overrides.retain(|binding| {
        if binding.keymap != preset || normalize_action_key(&binding.action) != normalized {
            return true;
        }
        match binding.context.as_deref() {
            Some(context) => KeyBindingCategory::from_id(context) != Some(category),
            None => false,
        }
    });
}

pub fn set_keybinding_overrides(
    settings: &mut Settings,
    preset: KeymapPreset,
    action: String,
    category: KeyBindingCategory,
    keystrokes: Vec<String>,
) {
    remove_keybinding_overrides(
        &mut settings.keybinding_overrides,
        preset,
        &action,
        category,
    );
    if keystrokes.is_empty() {
        settings.keybinding_overrides.push(KeyBindingOverride {
            keymap: preset,
            action,
            keystrokes: String::new(),
            context: Some(category.id().to_string()),
        });
    } else {
        for keystrokes in keystrokes {
            settings.keybinding_overrides.push(KeyBindingOverride {
                keymap: preset,
                action: action.clone(),
                keystrokes,
                context: Some(category.id().to_string()),
            });
        }
    }
}

pub fn reset_keybinding_overrides(
    settings: &mut Settings,
    preset: KeymapPreset,
    action: &str,
    category: KeyBindingCategory,
) {
    remove_keybinding_overrides(&mut settings.keybinding_overrides, preset, action, category);
}

#[cfg(target_os = "macos")]
fn platform_primary_modifier() -> &'static str {
    "cmd"
}

#[cfg(not(target_os = "macos"))]
fn platform_primary_modifier() -> &'static str {
    "ctrl"
}

fn context_without_overlays(base: &str) -> String {
    let mut predicate = base.to_string();
    for context in OVERLAY_CONTEXTS {
        if *context == base {
            continue;
        }
        predicate.push_str(" && !");
        predicate.push_str(context);
    }
    predicate
}

fn raw_default_keystrokes(spec: &CommandSpec, preset: KeymapPreset) -> &'static [&'static str] {
    match preset {
        KeymapPreset::Modern => spec.modern,
        KeymapPreset::Vim => spec.vim,
    }
}

fn default_keystrokes(spec: &CommandSpec, preset: KeymapPreset) -> Vec<String> {
    raw_default_keystrokes(spec, preset)
        .iter()
        .filter_map(|value| expand_platform_default_keystroke(value))
        .collect()
}

fn expand_platform_default_keystroke(keystrokes: &str) -> Option<String> {
    if let Some(keystrokes) = keystrokes.strip_prefix("mac:") {
        cfg!(target_os = "macos").then(|| expand_platform_keystroke(keystrokes))
    } else if let Some(keystrokes) = keystrokes.strip_prefix("nonmac:") {
        (!cfg!(target_os = "macos")).then(|| expand_platform_keystroke(keystrokes))
    } else {
        Some(expand_platform_keystroke(keystrokes))
    }
}

fn effective_keystrokes(
    spec: &CommandSpec,
    preset: KeymapPreset,
    overrides: &[KeyBindingOverride],
) -> Vec<String> {
    let matches = overrides
        .iter()
        .filter(|binding| binding.keymap == preset && override_applies_to_spec(binding, spec))
        .collect::<Vec<_>>();

    if matches.is_empty() {
        return default_keystrokes(spec, preset);
    }

    matches
        .into_iter()
        .filter_map(|binding| {
            let keystrokes = binding.keystrokes.trim();
            (!keystrokes.is_empty()).then(|| expand_platform_keystroke(keystrokes))
        })
        .collect()
}

fn override_applies_to_spec(binding: &KeyBindingOverride, spec: &CommandSpec) -> bool {
    if !action_matches_spec(&binding.action, spec) {
        return false;
    }

    match binding
        .context
        .as_deref()
        .and_then(KeyBindingCategory::from_id)
    {
        Some(category) => category == spec.category,
        None => spec.legacy_default,
    }
}

fn action_matches_spec(action_name: &str, spec: &CommandSpec) -> bool {
    let action_name = action_name.trim();
    let action = (spec.build)();
    if action.name() == action_name {
        return true;
    }

    let normalized = normalize_action_key(action_name);
    spec.aliases
        .iter()
        .chain(std::iter::once(&spec.action))
        .any(|alias| normalize_action_key(alias) == normalized)
}

pub fn validate_keybinding(
    cx: &mut App,
    action_name: &str,
    category: KeyBindingCategory,
    keystrokes: &str,
) -> Result<(), String> {
    let Some(spec) = command_specs()
        .iter()
        .find(|spec| spec.category == category && action_matches_spec(action_name, spec))
    else {
        return Ok(());
    };

    let context = KeyBindingContextPredicate::parse(&category.predicate())
        .map(Rc::new)
        .map_err(|error| format!("{error:#}"))?;
    let keystrokes = expand_platform_keystroke(keystrokes);

    KeyBinding::load(
        &keystrokes,
        (spec.build)(),
        Some(context),
        false,
        None,
        cx.keyboard_mapper().as_ref(),
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn bindings_for_unknown_override(cx: &mut App, binding: &KeyBindingOverride) -> Vec<KeyBinding> {
    let action_name = binding.action.trim();
    let keystrokes = binding.keystrokes.trim();
    if action_name.is_empty() || keystrokes.is_empty() {
        return Vec::new();
    }

    let action = match cx.build_action(action_name, None) {
        Ok(action) => action,
        Err(error) => {
            eprintln!("failed to build action for keybinding `{action_name}`: {error}");
            return Vec::new();
        }
    };

    let context = binding
        .context
        .as_deref()
        .and_then(KeyBindingCategory::from_id)
        .map(KeyBindingCategory::predicate);

    binding_for_action(cx, action_name, keystrokes, action, context.as_deref())
        .into_iter()
        .collect()
}

fn add_internal_bindings(cx: &mut App, bindings: &mut Vec<KeyBinding>) {
    add_many::<menu::Confirm>(cx, bindings, &["enter"], "Menu");
    add_many::<menu::Cancel>(cx, bindings, &["escape"], "Menu");
    add_many::<menu::SelectNext>(cx, bindings, &["down"], "Menu");
    add_many::<menu::SelectPrev>(cx, bindings, &["up"], "Menu");
    add_many::<menu::Remove>(cx, bindings, &["shift-delete"], "Menu");

    add_many::<SearchInputConfirm>(cx, bindings, &["enter"], "Search");
    add_many::<SearchInputPrevious>(cx, bindings, &["shift-enter"], "Search");
    add_many::<SearchInputCancel>(cx, bindings, &["escape"], "Search");
    add_many::<NoteCancel>(cx, bindings, &["escape"], "NoteEditor");
    add_many::<AiPanelClose>(cx, bindings, &["escape"], "AiPanel");
    add_many::<AiPanelClose>(cx, bindings, &["escape"], "QuickAsk");
    add_many::<OpenQuickAsk>(cx, bindings, &["tab"], "QuickAsk");
    add_many::<PanelClose>(cx, bindings, &["escape"], "SettingsPanel");
    add_many::<TogglePresentationMode>(cx, bindings, &["escape"], "PresentationMode");
    add_many::<CancelRegionSelection>(cx, bindings, &["escape"], "RegionSelection");
}

fn add_many<A>(cx: &mut App, bindings: &mut Vec<KeyBinding>, keys: &[&str], context: &str)
where
    A: Action + Default + 'static,
{
    for keystrokes in keys {
        add_binding::<A>(cx, bindings, keystrokes, context);
    }
}

fn add_binding<A>(cx: &mut App, bindings: &mut Vec<KeyBinding>, keystrokes: &str, context: &str)
where
    A: Action + Default + 'static,
{
    add_boxed_binding(
        cx,
        bindings,
        A::default().name(),
        keystrokes,
        Box::new(A::default()),
        Some(context),
    );
}

fn add_boxed_binding(
    cx: &mut App,
    bindings: &mut Vec<KeyBinding>,
    action_name: &str,
    keystrokes: &str,
    action: Box<dyn Action>,
    context: Option<&str>,
) {
    if let Some(binding) = binding_for_action(cx, action_name, keystrokes, action, context) {
        bindings.push(binding);
    }
}

fn binding_for_action(
    cx: &mut App,
    action_name: &str,
    keystrokes: &str,
    action: Box<dyn Action>,
    context: Option<&str>,
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

    let keystrokes = expand_platform_keystroke(keystrokes);

    match KeyBinding::load(
        &keystrokes,
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

fn boxed<A>() -> Box<dyn Action>
where
    A: Action + Default + 'static,
{
    Box::new(A::default())
}

fn command_specs() -> &'static [CommandSpec] {
    static SPECS: OnceLock<Vec<CommandSpec>> = OnceLock::new();

    SPECS
        .get_or_init(|| {
            vec![
                spec::<ToggleCommandPalette>(
                    "toggle-command-palette",
                    "Toggle Command Palette",
                    KeyBindingCategory::Workspace,
                    &["primary-shift-p"],
                    &["primary-shift-p"],
                    &["command-palette"],
                    true,
                ),
                spec::<OpenFile>(
                    "open-file",
                    "Open File",
                    KeyBindingCategory::Workspace,
                    &["primary-o"],
                    &["primary-o"],
                    &["open-pdf"],
                    true,
                ),
                spec::<OpenNewTab>(
                    "open-new-tab",
                    "Open New Tab",
                    KeyBindingCategory::Workspace,
                    &["primary-t"],
                    &["primary-t"],
                    &["new-tab"],
                    true,
                ),
                spec::<DuplicateTab>(
                    "duplicate-tab",
                    "Duplicate Tab",
                    KeyBindingCategory::Workspace,
                    &["primary-shift-d"],
                    &["primary-shift-d"],
                    &["duplicate-document-tab"],
                    true,
                ),
                spec::<CloseTab>(
                    "close-tab",
                    "Close Tab",
                    KeyBindingCategory::Workspace,
                    &["primary-w"],
                    &["primary-w"],
                    &[],
                    true,
                ),
                spec::<OpenSettings>(
                    "open-settings",
                    "Open Settings",
                    KeyBindingCategory::Workspace,
                    &["primary-,"],
                    &["primary-,"],
                    &["settings"],
                    true,
                ),
                spec::<OpenKeybindings>(
                    "open-keybindings",
                    "Open Keybindings",
                    KeyBindingCategory::Workspace,
                    &[],
                    &[],
                    &["open-keymap", "keybindings", "keymap"],
                    true,
                ),
                spec::<ToggleFullScreen>(
                    "toggle-full-screen",
                    "Toggle Full Screen",
                    KeyBindingCategory::Workspace,
                    &["f11"],
                    &["f11"],
                    &["toggle-fullscreen", "fullscreen"],
                    true,
                ),
                spec::<ToggleTransparency>(
                    "toggle-transparency",
                    "Toggle Transparency",
                    KeyBindingCategory::Workspace,
                    &["primary-shift-t"],
                    &["primary-shift-t"],
                    &["transparency"],
                    true,
                ),
                spec::<ToggleMode>(
                    "toggle-theme",
                    "Toggle Theme",
                    KeyBindingCategory::Workspace,
                    &["primary-k primary-shift-t"],
                    &["primary-k primary-shift-t"],
                    &["toggle-theme-mode", "toggle-theme"],
                    true,
                ),
                spec::<ToggleThemeSelector>(
                    "toggle-theme-selector",
                    "Toggle Theme Selector",
                    KeyBindingCategory::Workspace,
                    &["primary-k primary-t"],
                    &["primary-k primary-t"],
                    &["theme-selector", "select-theme"],
                    true,
                ),
                spec::<ToggleAiPanel>(
                    "toggle-ai-panel",
                    "Toggle AI Panel",
                    KeyBindingCategory::Workspace,
                    &["primary-shift-a"],
                    &["primary-shift-a"],
                    &["ai-panel", "ai", "assistant"],
                    true,
                ),
                spec::<OpenHistory>(
                    "open-history",
                    "Open History",
                    KeyBindingCategory::Workspace,
                    &["ctrl-h"],
                    &["ctrl-h"],
                    &["history", "recent-files"],
                    true,
                ),
                spec::<OpenBookmarks>(
                    "open-bookmarks",
                    "Open Bookmarks",
                    KeyBindingCategory::Workspace,
                    &["ctrl-b"],
                    &["'"],
                    &["bookmarks"],
                    true,
                ),
                spec::<ToggleThumbnailSidebar>(
                    "toggle-thumbnails",
                    "Toggle Page Thumbnails",
                    KeyBindingCategory::Workspace,
                    &["ctrl-shift-g"],
                    &["ctrl-shift-g"],
                    &["toggle-thumbnail-sidebar", "page-thumbnails"],
                    true,
                ),
                spec::<ToggleToc>(
                    "toggle-toc",
                    "Toggle Table of Contents",
                    KeyBindingCategory::Workspace,
                    &[],
                    &[],
                    &["toggle-table-of-contents", "table-of-contents"],
                    true,
                ),
                spec::<OpenNotes>(
                    "open-notes",
                    "Open Notes",
                    KeyBindingCategory::Workspace,
                    &[],
                    &[],
                    &["notes"],
                    true,
                ),
                spec::<ToggleNotes>(
                    "toggle-notes",
                    "Toggle Notes Sidebar",
                    KeyBindingCategory::Workspace,
                    &[],
                    &[],
                    &["toggle-notes-sidebar"],
                    true,
                ),
                spec::<SplitRight>(
                    "split-right",
                    "Split Right",
                    KeyBindingCategory::Workspace,
                    &["ctrl-\\", "ctrl-k right"],
                    &[],
                    &["pane-split-right"],
                    true,
                ),
                spec::<SplitLeft>(
                    "split-left",
                    "Split Left",
                    KeyBindingCategory::Workspace,
                    &["ctrl-k left"],
                    &[],
                    &["pane-split-left"],
                    true,
                ),
                spec::<SplitUp>(
                    "split-up",
                    "Split Up",
                    KeyBindingCategory::Workspace,
                    &["ctrl-k up"],
                    &[],
                    &["pane-split-up"],
                    true,
                ),
                spec::<SplitDown>(
                    "split-down",
                    "Split Down",
                    KeyBindingCategory::Workspace,
                    &["ctrl-k down"],
                    &[],
                    &["pane-split-down"],
                    true,
                ),
                spec::<SplitVertical>(
                    "split-vertical",
                    "Split Vertically",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w v", "ctrl-w ctrl-v"],
                    &["vsplit"],
                    true,
                ),
                spec::<SplitHorizontal>(
                    "split-horizontal",
                    "Split Horizontally",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w s", "ctrl-w ctrl-s", "ctrl-w shift-s"],
                    &["hsplit"],
                    true,
                ),
                spec::<ActivatePaneLeft>(
                    "activate-pane-left",
                    "Focus Pane Left",
                    KeyBindingCategory::Workspace,
                    &["ctrl-k ctrl-left"],
                    &["ctrl-w left", "ctrl-w h", "ctrl-w ctrl-h"],
                    &["pane-left"],
                    true,
                ),
                spec::<ActivatePaneRight>(
                    "activate-pane-right",
                    "Focus Pane Right",
                    KeyBindingCategory::Workspace,
                    &["ctrl-k ctrl-right"],
                    &["ctrl-w right", "ctrl-w l", "ctrl-w ctrl-l"],
                    &["pane-right"],
                    true,
                ),
                spec::<ActivatePaneUp>(
                    "activate-pane-up",
                    "Focus Pane Up",
                    KeyBindingCategory::Workspace,
                    &["ctrl-k ctrl-up"],
                    &["ctrl-w up", "ctrl-w k", "ctrl-w ctrl-k"],
                    &["pane-up"],
                    true,
                ),
                spec::<ActivatePaneDown>(
                    "activate-pane-down",
                    "Focus Pane Down",
                    KeyBindingCategory::Workspace,
                    &["ctrl-k ctrl-down"],
                    &["ctrl-w down", "ctrl-w j", "ctrl-w ctrl-j"],
                    &["pane-down"],
                    true,
                ),
                spec::<ActivateNextPane>(
                    "activate-next-pane",
                    "Focus Next Pane",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w w", "ctrl-w ctrl-w"],
                    &["next-pane"],
                    true,
                ),
                spec::<ActivatePreviousPane>(
                    "activate-previous-pane",
                    "Focus Previous Pane",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w p", "ctrl-w ctrl-p"],
                    &["previous-pane", "prev-pane"],
                    true,
                ),
                spec::<SwapPaneLeft>(
                    "swap-pane-left",
                    "Swap Pane Left",
                    KeyBindingCategory::Workspace,
                    &["ctrl-k shift-left"],
                    &["ctrl-w shift-left"],
                    &[],
                    true,
                ),
                spec::<SwapPaneRight>(
                    "swap-pane-right",
                    "Swap Pane Right",
                    KeyBindingCategory::Workspace,
                    &["ctrl-k shift-right"],
                    &["ctrl-w shift-right"],
                    &[],
                    true,
                ),
                spec::<SwapPaneUp>(
                    "swap-pane-up",
                    "Swap Pane Up",
                    KeyBindingCategory::Workspace,
                    &["ctrl-k shift-up"],
                    &["ctrl-w shift-up"],
                    &[],
                    true,
                ),
                spec::<SwapPaneDown>(
                    "swap-pane-down",
                    "Swap Pane Down",
                    KeyBindingCategory::Workspace,
                    &["ctrl-k shift-down"],
                    &["ctrl-w shift-down"],
                    &[],
                    true,
                ),
                spec::<MovePaneLeft>(
                    "move-pane-left",
                    "Move Pane Left",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w shift-h"],
                    &[],
                    true,
                ),
                spec::<MovePaneRight>(
                    "move-pane-right",
                    "Move Pane Right",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w shift-l"],
                    &[],
                    true,
                ),
                spec::<MovePaneUp>(
                    "move-pane-up",
                    "Move Pane Up",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w shift-k"],
                    &[],
                    true,
                ),
                spec::<MovePaneDown>(
                    "move-pane-down",
                    "Move Pane Down",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w shift-j"],
                    &[],
                    true,
                ),
                spec::<ResizePaneLeft>(
                    "resize-pane-left",
                    "Resize Pane Left",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w <"],
                    &[],
                    true,
                ),
                spec::<ResizePaneRight>(
                    "resize-pane-right",
                    "Resize Pane Right",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w >"],
                    &[],
                    true,
                ),
                spec::<ResizePaneUp>(
                    "resize-pane-up",
                    "Resize Pane Up",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w +"],
                    &[],
                    true,
                ),
                spec::<ResizePaneDown>(
                    "resize-pane-down",
                    "Resize Pane Down",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w -"],
                    &[],
                    true,
                ),
                spec::<ResetPaneSizes>(
                    "reset-pane-sizes",
                    "Reset Pane Sizes",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w ="],
                    &["equalize-panes"],
                    true,
                ),
                spec::<CloseOtherPanes>(
                    "close-other-panes",
                    "Close Other Panes",
                    KeyBindingCategory::Workspace,
                    &[],
                    &["ctrl-w o", "ctrl-w ctrl-o"],
                    &["only-pane"],
                    true,
                ),
                spec::<CopySelection>(
                    "copy-selection",
                    "Copy Selection",
                    KeyBindingCategory::DocumentView,
                    &["primary-c"],
                    &["primary-c"],
                    &["copy"],
                    true,
                ),
                spec::<Undo>(
                    "undo",
                    "Undo",
                    KeyBindingCategory::DocumentView,
                    &["primary-z"],
                    &["primary-z"],
                    &["undo"],
                    true,
                ),
                spec::<Redo>(
                    "redo",
                    "Redo",
                    KeyBindingCategory::DocumentView,
                    &["primary-shift-z", "nonmac:ctrl-y"],
                    &["primary-shift-z", "nonmac:ctrl-y"],
                    &["redo"],
                    true,
                ),
                spec::<TranslateSelection>(
                    "translate-selection",
                    "Translate Selection",
                    KeyBindingCategory::DocumentView,
                    &["primary-shift-l"],
                    &["primary-shift-l"],
                    &["translate", "translation"],
                    true,
                ),
                spec::<TranslateDocument>(
                    "translate-document",
                    "Translate Document",
                    KeyBindingCategory::DocumentView,
                    &[],
                    &[],
                    &["translate", "translation", "document", "whole-document"],
                    true,
                ),
                spec::<CancelDocumentTranslation>(
                    "cancel-document-translation",
                    "Cancel Document Translation",
                    KeyBindingCategory::DocumentView,
                    &[],
                    &[],
                    &["cancel", "translation", "document"],
                    true,
                ),
                spec::<ToggleRegionSelection>(
                    "toggle-region-selection",
                    "Toggle Region Selection",
                    KeyBindingCategory::DocumentView,
                    &[],
                    &[],
                    &["region", "crop", "screenshot"],
                    true,
                ),
                spec::<TogglePinnedTranslation>(
                    "toggle-pinned-translation",
                    "Toggle Pinned Translation",
                    KeyBindingCategory::DocumentView,
                    &[],
                    &[],
                    &["pinned-translation"],
                    true,
                ),
                spec::<SelectAll>(
                    "select-all",
                    "Select All",
                    KeyBindingCategory::DocumentView,
                    &["primary-a"],
                    &["primary-a"],
                    &[],
                    true,
                ),
                spec::<ZoomIn>(
                    "zoom-in",
                    "Zoom In",
                    KeyBindingCategory::DocumentView,
                    &["+", "shift-="],
                    &["+", "shift-="],
                    &[],
                    true,
                ),
                spec::<ZoomOut>(
                    "zoom-out",
                    "Zoom Out",
                    KeyBindingCategory::DocumentView,
                    &["-"],
                    &["-"],
                    &[],
                    true,
                ),
                spec::<ResetZoom>(
                    "reset-zoom",
                    "Reset Zoom",
                    KeyBindingCategory::DocumentView,
                    &["="],
                    &["="],
                    &[],
                    true,
                ),
                spec::<RotateClockwise>(
                    "rotate-clockwise",
                    "Rotate Clockwise",
                    KeyBindingCategory::DocumentView,
                    &["r"],
                    &["r"],
                    &["rotate", "rotate-cw"],
                    true,
                ),
                spec::<RotateCounterclockwise>(
                    "rotate-counterclockwise",
                    "Rotate Counterclockwise",
                    KeyBindingCategory::DocumentView,
                    &["shift-r"],
                    &["shift-r"],
                    &["rotate-ccw"],
                    true,
                ),
                spec::<TogglePublisherStyles>(
                    "toggle-publisher-styles",
                    "Toggle EPUB Publisher Styles",
                    KeyBindingCategory::DocumentView,
                    &[],
                    &[],
                    &["publisher-styles"],
                    true,
                ),
                spec::<Reload>(
                    "reload",
                    "Reload Document",
                    KeyBindingCategory::DocumentView,
                    &["ctrl-r"],
                    &["ctrl-r"],
                    &["reload-document"],
                    true,
                ),
                spec::<TogglePresentationMode>(
                    "toggle-presentation-mode",
                    "Toggle Presentation Mode",
                    KeyBindingCategory::DocumentView,
                    &["f5"],
                    &["f5"],
                    &["toggle-presentation", "presentation", "presentation-mode"],
                    true,
                ),
                spec::<Search>(
                    "search",
                    "Search",
                    KeyBindingCategory::DocumentView,
                    &["primary-f", "/"],
                    &["/"],
                    &[],
                    true,
                ),
                spec::<NextSearchResult>(
                    "next-search-result",
                    "Next Search Result",
                    KeyBindingCategory::DocumentView,
                    &["f3", "nonmac:ctrl-alt-g", "mac:cmd-g"],
                    &["n"],
                    &[
                        "search-next",
                        "search-next-result",
                        "select-next-match",
                        "search::SelectNextMatch",
                    ],
                    true,
                ),
                spec::<PrevSearchResult>(
                    "prev-search-result",
                    "Previous Search Result",
                    KeyBindingCategory::DocumentView,
                    &["shift-f3", "nonmac:ctrl-alt-shift-g", "mac:cmd-shift-g"],
                    &["shift-n"],
                    &[
                        "previous-search-result",
                        "search-prev",
                        "search-previous",
                        "search-previous-result",
                        "select-prev-match",
                        "select-previous-match",
                        "search::SelectPrevMatch",
                        "search::SelectPreviousMatch",
                    ],
                    true,
                ),
                spec::<NextSearchResult>(
                    "next-search-result",
                    "Next Search Result",
                    KeyBindingCategory::SearchResults,
                    &["primary-g"],
                    &["ctrl-g"],
                    &["search-next", "search-next-result"],
                    false,
                ),
                spec::<PrevSearchResult>(
                    "prev-search-result",
                    "Previous Search Result",
                    KeyBindingCategory::SearchResults,
                    &["primary-shift-g"],
                    &["ctrl-shift-g"],
                    &["search-prev", "search-previous"],
                    false,
                ),
                spec::<NextPage>(
                    "next-page",
                    "Next Page",
                    KeyBindingCategory::DocumentView,
                    &["pagedown"],
                    &["ctrl-f"],
                    &[],
                    true,
                ),
                spec::<PreviousPage>(
                    "previous-page",
                    "Previous Page",
                    KeyBindingCategory::DocumentView,
                    &["pageup"],
                    &["ctrl-b"],
                    &["prev-page"],
                    true,
                ),
                spec::<NextHalfPage>(
                    "next-half-page",
                    "Next Half Page",
                    KeyBindingCategory::DocumentView,
                    &["shift-pagedown"],
                    &["ctrl-d"],
                    &[],
                    true,
                ),
                spec::<PreviousHalfPage>(
                    "previous-half-page",
                    "Previous Half Page",
                    KeyBindingCategory::DocumentView,
                    &["shift-pageup"],
                    &["ctrl-u"],
                    &["prev-half-page"],
                    true,
                ),
                spec::<ScrollLineDown>(
                    "scroll-line-down",
                    "Scroll Line Down",
                    KeyBindingCategory::DocumentView,
                    &["down"],
                    &["j", "down"],
                    &["scroll-down", "move-down", "move-visual-mark-down"],
                    true,
                ),
                spec::<ScrollLineUp>(
                    "scroll-line-up",
                    "Scroll Line Up",
                    KeyBindingCategory::DocumentView,
                    &["up"],
                    &["k", "up"],
                    &["scroll-up", "move-up", "move-visual-mark-up"],
                    true,
                ),
                spec::<GoToBeginning>(
                    "go-to-beginning",
                    "Go to Beginning",
                    KeyBindingCategory::DocumentView,
                    &["home", "ctrl-home", "alt-<"],
                    &["home", "ctrl-home", "alt-<", "g g"],
                    &[
                        "go-to-start",
                        "go-to-top",
                        "go-to-the-beginning",
                        "start-of-document",
                    ],
                    true,
                ),
                spec::<GoToEnd>(
                    "go-to-end",
                    "Go to End",
                    KeyBindingCategory::DocumentView,
                    &["end", "ctrl-end", "alt->"],
                    &["end", "ctrl-end", "alt->", "shift-g"],
                    &["go-to-bottom", "go-to-the-end", "end-of-document"],
                    true,
                ),
                spec::<JumpBackward>(
                    "jump-backward",
                    "Jump Backward",
                    KeyBindingCategory::DocumentView,
                    &[],
                    &["ctrl-o"],
                    &["jump-back"],
                    true,
                ),
                spec::<JumpForward>(
                    "jump-forward",
                    "Jump Forward",
                    KeyBindingCategory::DocumentView,
                    &[],
                    &["ctrl-i"],
                    &[],
                    true,
                ),
                spec::<Toc>(
                    "toc",
                    "Open Table of Contents",
                    KeyBindingCategory::DocumentView,
                    &[],
                    &[],
                    &["goto-toc", "goto-table-of-contents"],
                    true,
                ),
                spec::<OpenQuickAsk>(
                    "open-quick-ask",
                    "Open Quick Ask",
                    KeyBindingCategory::DocumentView,
                    &["tab"],
                    &["tab"],
                    &["quick-ask"],
                    true,
                ),
                spec::<OpenGoToPage>(
                    "go-to-page",
                    "Go to Page",
                    KeyBindingCategory::DocumentView,
                    &[],
                    &[":"],
                    &["goto-page", "open-go-to-page"],
                    true,
                ),
                spec::<AddBookmark>(
                    "add-bookmark",
                    "Add Bookmark",
                    KeyBindingCategory::DocumentView,
                    &["ctrl-d"],
                    &["m"],
                    &["bookmark-page"],
                    true,
                ),
                spec::<ToggleGridOverview>(
                    "toggle-grid-overview",
                    "Toggle Page Overview",
                    KeyBindingCategory::DocumentView,
                    &["ctrl-g"],
                    &["ctrl-g"],
                    &["grid-overview", "page-overview"],
                    true,
                ),
                spec::<Search>(
                    "search",
                    "Search",
                    KeyBindingCategory::PageOverview,
                    &["primary-f", "/"],
                    &["/"],
                    &[],
                    false,
                ),
                spec::<NextSearchResult>(
                    "next-search-result",
                    "Next Search Result",
                    KeyBindingCategory::PageOverview,
                    &["f3", "nonmac:ctrl-alt-g", "mac:cmd-g"],
                    &["n"],
                    &[
                        "search-next",
                        "search-next-result",
                        "select-next-match",
                        "search::SelectNextMatch",
                    ],
                    false,
                ),
                spec::<PrevSearchResult>(
                    "prev-search-result",
                    "Previous Search Result",
                    KeyBindingCategory::PageOverview,
                    &["shift-f3", "nonmac:ctrl-alt-shift-g", "mac:cmd-shift-g"],
                    &["shift-n"],
                    &[
                        "previous-search-result",
                        "search-prev",
                        "search-previous",
                        "search-previous-result",
                        "select-prev-match",
                        "select-previous-match",
                        "search::SelectPrevMatch",
                        "search::SelectPreviousMatch",
                    ],
                    false,
                ),
                spec::<ToggleCommandPalette>(
                    "toggle-command-palette",
                    "Toggle Command Palette",
                    KeyBindingCategory::PageOverview,
                    &["primary-shift-p"],
                    &["primary-shift-p"],
                    &["command-palette"],
                    false,
                ),
                spec::<ZoomIn>(
                    "zoom-in",
                    "Zoom In",
                    KeyBindingCategory::PageOverview,
                    &["+", "shift-="],
                    &["+", "shift-="],
                    &[],
                    false,
                ),
                spec::<ZoomOut>(
                    "zoom-out",
                    "Zoom Out",
                    KeyBindingCategory::PageOverview,
                    &["-"],
                    &["-"],
                    &[],
                    false,
                ),
                spec::<NextPage>(
                    "next-page",
                    "Next Page",
                    KeyBindingCategory::PageOverview,
                    &["pagedown"],
                    &["ctrl-f"],
                    &[],
                    false,
                ),
                spec::<PreviousPage>(
                    "previous-page",
                    "Previous Page",
                    KeyBindingCategory::PageOverview,
                    &["pageup"],
                    &["ctrl-b"],
                    &["prev-page"],
                    false,
                ),
                spec::<NextHalfPage>(
                    "next-half-page",
                    "Next Half Page",
                    KeyBindingCategory::PageOverview,
                    &["shift-pagedown"],
                    &["ctrl-d"],
                    &[],
                    false,
                ),
                spec::<PreviousHalfPage>(
                    "previous-half-page",
                    "Previous Half Page",
                    KeyBindingCategory::PageOverview,
                    &["shift-pageup"],
                    &["ctrl-u"],
                    &["prev-half-page"],
                    false,
                ),
                spec::<GoToBeginning>(
                    "go-to-beginning",
                    "Go to Beginning",
                    KeyBindingCategory::PageOverview,
                    &["home", "ctrl-home", "alt-<"],
                    &["home", "ctrl-home", "alt-<", "g g"],
                    &["go-to-start", "go-to-top"],
                    false,
                ),
                spec::<GoToEnd>(
                    "go-to-end",
                    "Go to End",
                    KeyBindingCategory::PageOverview,
                    &["end", "ctrl-end", "alt->"],
                    &["end", "ctrl-end", "alt->", "shift-g"],
                    &["go-to-bottom"],
                    false,
                ),
                spec::<GridCursorUp>(
                    "grid-cursor-up",
                    "Move Up",
                    KeyBindingCategory::PageOverview,
                    &["up"],
                    &["up"],
                    &["grid-overview-cursor-up"],
                    true,
                ),
                spec::<GridCursorDown>(
                    "grid-cursor-down",
                    "Move Down",
                    KeyBindingCategory::PageOverview,
                    &["down"],
                    &["down"],
                    &["grid-overview-cursor-down"],
                    true,
                ),
                spec::<GridCursorLeft>(
                    "grid-cursor-left",
                    "Move Left",
                    KeyBindingCategory::PageOverview,
                    &["left"],
                    &["left"],
                    &["grid-overview-cursor-left"],
                    true,
                ),
                spec::<GridCursorRight>(
                    "grid-cursor-right",
                    "Move Right",
                    KeyBindingCategory::PageOverview,
                    &["right"],
                    &["right"],
                    &["grid-overview-cursor-right"],
                    true,
                ),
                spec::<GridOverviewConfirm>(
                    "grid-overview-confirm",
                    "Open Selected Page",
                    KeyBindingCategory::PageOverview,
                    &["enter"],
                    &["enter"],
                    &["grid-overview-open", "open-grid-overview-page"],
                    true,
                ),
                spec::<ToggleGridOverview>(
                    "toggle-grid-overview",
                    "Close Page Overview",
                    KeyBindingCategory::PageOverview,
                    &["ctrl-g", "escape"],
                    &["ctrl-g", "escape"],
                    &["grid-overview", "page-overview"],
                    false,
                ),
                spec::<ScrollLineDown>(
                    "scroll-line-down",
                    "Scroll Line Down",
                    KeyBindingCategory::NewTab,
                    &[],
                    &[],
                    &["scroll-down", "move-down", "move-visual-mark-down"],
                    false,
                ),
                spec::<ScrollLineUp>(
                    "scroll-line-up",
                    "Scroll Line Up",
                    KeyBindingCategory::NewTab,
                    &[],
                    &[],
                    &["scroll-up", "move-up", "move-visual-mark-up"],
                    false,
                ),
                spec::<CursorUp>(
                    "cursor-up",
                    "Move Focus Up",
                    KeyBindingCategory::NewTab,
                    &["up"],
                    &["k", "up"],
                    &[],
                    false,
                ),
                spec::<CursorDown>(
                    "cursor-down",
                    "Move Focus Down",
                    KeyBindingCategory::NewTab,
                    &["down"],
                    &["j", "down"],
                    &[],
                    false,
                ),
                spec::<CursorLeft>(
                    "cursor-left",
                    "Move Focus Left",
                    KeyBindingCategory::NewTab,
                    &["left"],
                    &["h", "left"],
                    &[],
                    false,
                ),
                spec::<CursorRight>(
                    "cursor-right",
                    "Move Focus Right",
                    KeyBindingCategory::NewTab,
                    &["right"],
                    &["l", "right"],
                    &[],
                    false,
                ),
                spec::<CursorFirst>(
                    "cursor-first",
                    "Move Focus to First Item",
                    KeyBindingCategory::NewTab,
                    &["home"],
                    &["g g", "home"],
                    &[],
                    false,
                ),
                spec::<CursorLast>(
                    "cursor-last",
                    "Move Focus to Last Item",
                    KeyBindingCategory::NewTab,
                    &["end"],
                    &["shift-g", "end"],
                    &[],
                    false,
                ),
                spec::<ExtendSelectionUp>(
                    "extend-selection-up",
                    "Extend Selection Up",
                    KeyBindingCategory::NewTab,
                    &["shift-up"],
                    &["shift-k", "shift-up"],
                    &[],
                    false,
                ),
                spec::<ExtendSelectionDown>(
                    "extend-selection-down",
                    "Extend Selection Down",
                    KeyBindingCategory::NewTab,
                    &["shift-down"],
                    &["shift-j", "shift-down"],
                    &[],
                    false,
                ),
                spec::<ExtendSelectionLeft>(
                    "extend-selection-left",
                    "Extend Selection Left",
                    KeyBindingCategory::NewTab,
                    &["shift-left"],
                    &["shift-h", "shift-left"],
                    &[],
                    false,
                ),
                spec::<ExtendSelectionRight>(
                    "extend-selection-right",
                    "Extend Selection Right",
                    KeyBindingCategory::NewTab,
                    &["shift-right"],
                    &["shift-l", "shift-right"],
                    &[],
                    false,
                ),
                spec::<ExtendSelectionFirst>(
                    "extend-selection-first",
                    "Extend Selection to First Item",
                    KeyBindingCategory::NewTab,
                    &["shift-home"],
                    &["shift-home"],
                    &[],
                    false,
                ),
                spec::<ExtendSelectionLast>(
                    "extend-selection-last",
                    "Extend Selection to Last Item",
                    KeyBindingCategory::NewTab,
                    &["shift-end"],
                    &["shift-end"],
                    &[],
                    false,
                ),
                spec::<ActivateFocused>(
                    "activate-focused",
                    "Open Focused Item",
                    KeyBindingCategory::NewTab,
                    &["enter"],
                    &["enter"],
                    &[],
                    false,
                ),
                spec::<ToggleFocused>(
                    "toggle-focused",
                    "Toggle Focused Item",
                    KeyBindingCategory::NewTab,
                    &["space"],
                    &["space"],
                    &[],
                    false,
                ),
                spec::<NewTabSelectAll>(
                    "select-all",
                    "Select All Recent Documents",
                    KeyBindingCategory::NewTab,
                    &["primary-a"],
                    &["primary-a"],
                    &[],
                    false,
                ),
                spec::<NextHalfPage>(
                    "next-half-page",
                    "Next Half Page",
                    KeyBindingCategory::NewTab,
                    &["shift-pagedown"],
                    &["ctrl-d"],
                    &[],
                    false,
                ),
                spec::<PreviousHalfPage>(
                    "previous-half-page",
                    "Previous Half Page",
                    KeyBindingCategory::NewTab,
                    &["shift-pageup"],
                    &["ctrl-u"],
                    &["prev-half-page"],
                    false,
                ),
                spec::<EnterSelect>(
                    "enter-select",
                    "Enter Selection Mode",
                    KeyBindingCategory::NewTab,
                    &[],
                    &[],
                    &[],
                    false,
                ),
                spec::<CancelSelect>(
                    "cancel-select",
                    "Cancel Selection",
                    KeyBindingCategory::NewTab,
                    &["escape"],
                    &["escape"],
                    &[],
                    false,
                ),
                spec::<DeleteSelected>(
                    "delete-selected",
                    "Remove Selected from Recents",
                    KeyBindingCategory::NewTab,
                    &["delete", "backspace"],
                    &["delete", "backspace"],
                    &[],
                    false,
                ),
            ]
        })
        .as_slice()
}

fn spec<A>(
    action: &'static str,
    label: &'static str,
    category: KeyBindingCategory,
    modern: &'static [&'static str],
    vim: &'static [&'static str],
    aliases: &'static [&'static str],
    legacy_default: bool,
) -> CommandSpec
where
    A: Action + Default + 'static,
{
    CommandSpec {
        action,
        label,
        aliases,
        category,
        modern,
        vim,
        legacy_default,
        build: boxed::<A>,
    }
}

#[cfg(test)]
mod tests;
