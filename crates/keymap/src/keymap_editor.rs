use std::{cmp::Ordering, ops::Range};

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, Focusable, Half, IntoElement,
    Keystroke, MouseButton, MouseDownEvent, ParentElement, Render, ScrollStrategy, SharedString,
    Styled, UniformListScrollHandle, Window, div, prelude::*, px, relative, transparent_black,
    uniform_list,
};

use crate::{
    icons::IconName,
    keybindings::{self, EditableKeyBinding, KeyBindingCategory},
    keystroke_recorder::{KeystrokeRecorder, KeystrokeRecorderEvent},
    scrollbar::WithScrollbar as _,
    settings::{KeymapPreset, Settings, SettingsStore, ui_rems},
};
use glyphst_ui::tooltip::text_tooltip;
use gpui_component::{
    ActiveTheme as _, IndexPath, Sizable as _, ThemeColor as Theme, WindowExt as _,
    alert::Alert,
    button::{Button, ButtonVariants as _},
    dialog::DialogFooter,
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    menu::{ContextMenuExt as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    searchable_list::SearchableListItem,
    select::{Select, SelectEvent, SelectState},
};

const ROW_HEIGHT: f32 = 36.0;
const ICON_COL: f32 = 36.0;
const CONTEXT_COL: f32 = 170.0;
const SOURCE_COL: f32 = 90.0;

fn format_keybinding_sequence(sequence: impl AsRef<str>) -> String {
    sequence
        .as_ref()
        .split_whitespace()
        .map(|stroke| {
            Keystroke::parse(stroke)
                .map(|stroke| Kbd::format(&stroke))
                .unwrap_or_else(|_| stroke.to_string())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_keybinding_sequence(sequence: impl AsRef<str>, cx: &App) -> AnyElement {
    div()
        .text_color(cx.theme().muted_foreground)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().tokens.background)
        .py_0p5()
        .px_1()
        .min_w_5()
        .text_center()
        .rounded(cx.theme().radius.half())
        .line_height(relative(1.))
        .text_xs()
        .whitespace_normal()
        .flex_shrink_0()
        .child(format_keybinding_sequence(sequence))
        .into_any_element()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SortColumn {
    Category,
    Action,
    Keystrokes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputSeverity {
    Warning,
    Error,
}

#[derive(Clone)]
struct InputError {
    severity: InputSeverity,
    message: SharedString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindingInputMode {
    Add,
    Replace(usize),
}

#[derive(Clone)]
struct KeymapOption {
    preset: KeymapPreset,
    label: SharedString,
}

impl SearchableListItem for KeymapOption {
    type Value = KeymapPreset;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.preset
    }
}

struct EditModal {
    focus_handle: FocusHandle,
    keymap: KeymapPreset,
    action: String,
    label: String,
    category: KeyBindingCategory,
    defaults: Vec<String>,
    draft: Vec<String>,
    input_mode: Option<BindingInputMode>,
    input: Entity<KeystrokeRecorder>,
    error: Option<InputError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DraftBindingError {
    Duplicate,
    MissingTarget,
}

fn stage_draft_binding(
    draft: &mut Vec<String>,
    mode: BindingInputMode,
    candidate: String,
) -> Result<(), DraftBindingError> {
    let replacing = match mode {
        BindingInputMode::Add => None,
        BindingInputMode::Replace(index) => Some(index),
    };
    if draft
        .iter()
        .enumerate()
        .any(|(index, binding)| Some(index) != replacing && binding == &candidate)
    {
        return Err(DraftBindingError::Duplicate);
    }

    match mode {
        BindingInputMode::Add => draft.push(candidate),
        BindingInputMode::Replace(index) => {
            let Some(binding) = draft.get_mut(index) else {
                return Err(DraftBindingError::MissingTarget);
            };
            *binding = candidate;
        }
    }
    Ok(())
}

pub struct KeymapEditor {
    focus_handle: FocusHandle,
    filter_input: Entity<InputState>,
    keymap_select: Entity<SelectState<Vec<KeymapOption>>>,
    editing_preset: KeymapPreset,
    query: SharedString,
    scroll: UniformListScrollHandle,
    sort_column: SortColumn,
    sort_ascending: bool,
    selected_index: Option<usize>,
    editing: Option<EditModal>,
    save_error: Option<SharedString>,
}

impl KeymapEditor {
    pub fn new(focus_handle: FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editing_preset = cx.global::<SettingsStore>().settings.keymap_preset;
        let filter_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::fl!("keymap-search-placeholder"))
        });
        let keymap_options = vec![
            KeymapOption {
                preset: KeymapPreset::Modern,
                label: KeymapPreset::Modern.label().into(),
            },
            KeymapOption {
                preset: KeymapPreset::Vim,
                label: KeymapPreset::Vim.label().into(),
            },
        ];
        let selected_preset_index = if editing_preset == KeymapPreset::Vim {
            1
        } else {
            0
        };
        let keymap_select = cx.new(|cx| {
            SelectState::new(
                keymap_options,
                Some(IndexPath::new(selected_preset_index)),
                window,
                cx,
            )
        });
        cx.subscribe(&filter_input, |this, input, event, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            this.query = input.read(cx).value();
            this.selected_index = None;
            cx.notify();
        })
        .detach();
        cx.subscribe_in(&keymap_select, window, |this, _, event, _, cx| {
            let SelectEvent::Confirm(preset) = event;
            if let Some(preset) = preset {
                this.set_editing_preset(*preset, cx);
            }
        })
        .detach();
        cx.observe_global::<SettingsStore>(|_, cx| cx.notify())
            .detach();

        Self {
            focus_handle,
            filter_input,
            keymap_select,
            editing_preset,
            query: SharedString::default(),
            scroll: UniformListScrollHandle::new(),
            sort_column: SortColumn::Category,
            sort_ascending: true,
            selected_index: None,
            editing: None,
            save_error: None,
        }
    }

    pub fn focus_filter(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.filter_input.read(cx).focus_handle(cx);
        window.defer(cx, move |window, cx| {
            handle.focus(window, cx);
        });
    }

    fn set_editing_preset(&mut self, preset: KeymapPreset, cx: &mut Context<Self>) {
        if self.editing_preset == preset {
            return;
        }
        self.editing_preset = preset;
        self.selected_index = None;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn filtered_bindings(&self, settings: &Settings) -> Vec<EditableKeyBinding> {
        let query = self.query.trim().to_ascii_lowercase();
        let mut bindings =
            keybindings::editable_bindings(self.editing_preset, &settings.keybinding_overrides)
                .into_iter()
                .filter(|binding| {
                    query.is_empty()
                        || binding.label.to_ascii_lowercase().contains(&query)
                        || binding.action.contains(&query)
                        || binding
                            .category
                            .label()
                            .to_ascii_lowercase()
                            .contains(&query)
                        || binding.category.id().contains(&query)
                        || binding
                            .keystrokes
                            .iter()
                            .any(|key| key.to_ascii_lowercase().contains(&query))
                })
                .collect::<Vec<_>>();

        bindings.sort_by(|left, right| self.compare_bindings(left, right));
        bindings
    }

    fn compare_bindings(&self, left: &EditableKeyBinding, right: &EditableKeyBinding) -> Ordering {
        let ordering = match self.sort_column {
            SortColumn::Category => Self::category_rank(left.category)
                .cmp(&Self::category_rank(right.category))
                .then_with(|| left.label.cmp(&right.label)),
            SortColumn::Action => left.label.cmp(&right.label),
            SortColumn::Keystrokes => left.keystrokes.join(" ").cmp(&right.keystrokes.join(" ")),
        };

        if self.sort_ascending {
            ordering
        } else {
            ordering.reverse()
        }
    }

    fn category_rank(category: KeyBindingCategory) -> usize {
        keybindings::editable_categories()
            .iter()
            .position(|candidate| *candidate == category)
            .unwrap_or(usize::MAX)
    }

    fn set_sort(&mut self, column: SortColumn, cx: &mut Context<Self>) {
        if self.sort_column == column {
            self.sort_ascending = !self.sort_ascending;
        } else {
            self.sort_column = column;
            self.sort_ascending = true;
        }
        self.selected_index = None;
        cx.notify();
    }

    fn select_row(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected_index = Some(index);
        cx.notify();
    }

    fn copy_to_clipboard(&mut self, value: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(value));
    }

    fn view_matching(&mut self, keystroke: String, window: &mut Window, cx: &mut Context<Self>) {
        self.query = keystroke.clone().into();
        self.selected_index = None;
        self.filter_input.update(cx, |input, cx| {
            input.set_value(keystroke, window, cx);
        });
        self.editing = None;
        cx.notify();
    }

    fn persist_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let result = cx.global_mut::<SettingsStore>().set_settings(settings);
        let (keymap_preset, keybinding_overrides) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.keymap_preset,
                store.settings.keybinding_overrides.clone(),
            )
        };
        keybindings::apply(cx, keymap_preset, &keybinding_overrides);
        cx.refresh_windows();
        self.save_error = result.err().map(|error| format!("{error:#}").into());
        cx.notify();
    }

    fn start_editing(
        &mut self,
        action: String,
        label: String,
        category: KeyBindingCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keymap = self.editing_preset;
        let settings = &cx.global::<SettingsStore>().settings;
        let Some(binding) = keybindings::editable_bindings(keymap, &settings.keybinding_overrides)
            .into_iter()
            .find(|binding| binding.action == action && binding.category == category)
        else {
            return;
        };
        let input = cx.new(KeystrokeRecorder::new);
        cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
            KeystrokeRecorderEvent::Changed(_) => {
                if let Some(editing) = this.editing.as_mut() {
                    editing.error = None;
                }
                cx.notify();
            }
            KeystrokeRecorderEvent::Cancel => {
                this.cancel_editing(cx);
                window.close_dialog(cx);
            }
        })
        .detach();
        let focus_handle = cx.focus_handle();
        self.editing = Some(EditModal {
            focus_handle: focus_handle.clone(),
            keymap,
            action,
            label,
            category,
            defaults: binding.default_keystrokes,
            draft: binding.keystrokes,
            input_mode: None,
            input: input.clone(),
            error: None,
        });

        let editor = cx.entity();
        let content_editor = editor.clone();
        let save_button_editor = editor.clone();
        let cancel_button_editor = editor.clone();
        let save_action_editor = editor.clone();
        let close_editor = editor;
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .width(px(600.0))
                .title(crate::fl!("keymap-manage-keybindings"))
                .content({
                    let editor = content_editor.clone();
                    move |content, window, cx| {
                        let body =
                            editor.update(cx, |this, cx| this.render_editing_content(window, cx));
                        content.child(body)
                    }
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("keymap-cancel-editing")
                                .label(crate::fl!("cancel"))
                                .outline()
                                .on_click({
                                    let editor = cancel_button_editor.clone();
                                    move |_, window, cx| {
                                        editor.update(cx, |this, cx| this.cancel_editing(cx));
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(
                            Button::new("keymap-save-editing")
                                .label(crate::fl!("save"))
                                .primary()
                                .on_click({
                                    let editor = save_button_editor.clone();
                                    move |_, window, cx| {
                                        let should_close = editor
                                            .update(cx, |this, cx| this.save_editing(window, cx));
                                        if should_close {
                                            window.close_dialog(cx);
                                        }
                                    }
                                }),
                        ),
                )
                .on_ok({
                    let editor = save_action_editor.clone();
                    move |_, window, cx| editor.update(cx, |this, cx| this.save_editing(window, cx))
                })
                .on_close({
                    let editor = close_editor.clone();
                    move |_, _, cx| {
                        editor.update(cx, |this, cx| this.cancel_editing(cx));
                    }
                })
        });
        window.defer(cx, move |window, cx| {
            focus_handle.focus(window, cx);
        });
        cx.notify();
    }

    fn cancel_editing(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        cx.notify();
    }

    fn begin_binding_input(
        &mut self,
        mode: BindingInputMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        editing.input_mode = Some(mode);
        editing.error = None;
        let input = editing.input.clone();
        input.update(cx, |input, cx| input.clear(cx));
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    fn cancel_binding_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        editing.input_mode = None;
        editing.error = None;
        let input = editing.input.clone();
        let focus_handle = editing.focus_handle.clone();
        input.update(cx, |input, cx| input.clear(cx));
        focus_handle.focus(window, cx);
        cx.notify();
    }

    fn stage_binding_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing) = &self.editing else {
            return true;
        };
        let Some(mode) = editing.input_mode else {
            return true;
        };
        let action = editing.action.clone();
        let category = editing.category;
        let input = editing.input.clone();
        let focus_handle = editing.focus_handle.clone();
        let keys = editing
            .input
            .read(cx)
            .keystrokes()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        if keys.is_empty() {
            self.set_input_error(
                InputSeverity::Warning,
                crate::fl!("keymap-error-press-shortcut").into(),
                cx,
            );
            return false;
        }

        let keystroke = keys.join(" ");
        if let Err(error) = keybindings::validate_keybinding(cx, &action, category, &keystroke) {
            self.set_input_error(InputSeverity::Error, error.into(), cx);
            return false;
        }

        let result = self
            .editing
            .as_mut()
            .map(|editing| stage_draft_binding(&mut editing.draft, mode, keystroke))
            .unwrap_or(Ok(()));
        if let Err(error) = result {
            let message = match error {
                DraftBindingError::Duplicate => crate::fl!("keymap-error-duplicate-binding"),
                DraftBindingError::MissingTarget => crate::fl!("keymap-error-binding-changed"),
            };
            self.set_input_error(InputSeverity::Error, message.into(), cx);
            return false;
        }

        if let Some(editing) = self.editing.as_mut() {
            editing.input_mode = None;
            editing.error = None;
        }
        input.update(cx, |input, cx| input.clear(cx));
        focus_handle.focus(window, cx);
        cx.notify();
        true
    }

    fn remove_draft_binding(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        if index >= editing.draft.len() {
            return;
        }
        editing.draft.remove(index);
        editing.input_mode = match editing.input_mode {
            Some(BindingInputMode::Replace(replacing)) if replacing == index => None,
            Some(BindingInputMode::Replace(replacing)) if replacing > index => {
                Some(BindingInputMode::Replace(replacing - 1))
            }
            mode => mode,
        };
        editing.error = None;
        let focus_handle = editing.focus_handle.clone();
        focus_handle.focus(window, cx);
        cx.notify();
    }

    fn reset_draft_bindings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        editing.draft.clone_from(&editing.defaults);
        editing.input_mode = None;
        editing.error = None;
        let input = editing.input.clone();
        let focus_handle = editing.focus_handle.clone();
        input.update(cx, |input, cx| input.clear(cx));
        focus_handle.focus(window, cx);
        cx.notify();
    }

    fn save_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.stage_binding_input(window, cx) {
            return false;
        }
        let Some(editing) = &self.editing else {
            return true;
        };
        let keymap = editing.keymap;
        let action = editing.action.clone();
        let category = editing.category;
        let defaults = editing.defaults.clone();
        let draft = editing.draft.clone();

        let mut settings = cx.global::<SettingsStore>().settings.clone();
        if draft == defaults {
            keybindings::reset_keybinding_overrides(&mut settings, keymap, &action, category);
        } else {
            keybindings::set_keybinding_overrides(&mut settings, keymap, action, category, draft);
        }
        self.editing = None;
        self.persist_settings(settings, cx);
        true
    }

    fn set_input_error(
        &mut self,
        severity: InputSeverity,
        message: SharedString,
        cx: &mut Context<Self>,
    ) {
        if let Some(editing) = self.editing.as_mut() {
            editing.error = Some(InputError { severity, message });
        }
        cx.notify();
    }

    fn reset_binding(
        &mut self,
        action: String,
        category: KeyBindingCategory,
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        keybindings::reset_keybinding_overrides(
            &mut settings,
            self.editing_preset,
            &action,
            category,
        );
        self.persist_settings(settings, cx);
    }

    fn matching_bindings_count(
        &self,
        settings: &Settings,
        preset: KeymapPreset,
        category: KeyBindingCategory,
        action: &str,
        keystroke: &str,
    ) -> usize {
        if keystroke.is_empty() {
            return 0;
        }

        keybindings::editable_bindings(preset, &settings.keybinding_overrides)
            .into_iter()
            .filter(|binding| {
                binding.category == category
                    && binding.action != action
                    && binding.keystrokes.iter().any(|key| key == keystroke)
            })
            .count()
    }

    fn header_cell(
        &self,
        label: String,
        column: Option<SortColumn>,
        width: Option<f32>,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let suffix = match column {
            Some(column) if self.sort_column == column => {
                if self.sort_ascending {
                    " ↑"
                } else {
                    " ↓"
                }
            }
            _ => "",
        };

        let base = div()
            .h(px(32.0))
            .px_2()
            .flex()
            .items_center()
            .text_size(ui_rems(12.0))
            .text_color(theme.muted_foreground);
        let base = match width {
            Some(width) => base.w(px(width)).flex_none(),
            None => base.flex_1().min_w_0(),
        };

        match column {
            Some(column) => base
                .id(format!("keymap-column-{column:?}"))
                .hover(move |style| style.bg(theme.list_hover))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.set_sort(column, cx);
                        cx.stop_propagation();
                    }),
                )
                .child(format!("{label}{suffix}"))
                .into_any_element(),
            None => base.child(label).into_any_element(),
        }
    }

    fn render_row(
        &self,
        index: usize,
        binding: EditableKeyBinding,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let action = binding.action.to_string();
        let label = binding.label.to_string();
        let category = binding.category;
        let keystrokes = binding.keystrokes.clone();
        let is_overridden = binding.keystrokes != binding.default_keystrokes;
        let is_unbound = keystrokes.is_empty();
        let is_selected = self.selected_index == Some(index);

        let tooltip_text = if binding.default_keystrokes.is_empty() {
            format!("{}\n{}", binding.action, category.description())
        } else {
            let defaults = binding
                .default_keystrokes
                .iter()
                .map(format_keybinding_sequence)
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{}\n{}\nDefault: {}",
                binding.action,
                category.description(),
                defaults
            )
        };

        let source_label = if is_overridden {
            crate::fl!("keymap-source-user")
        } else {
            crate::fl!("keymap-source-default")
        };

        let row_action = action.clone();
        let row_label = label.clone();
        let cell_label = label.clone();
        let editor = cx.entity();
        let manage_button_editor = editor.clone();
        let edit_editor = editor.clone();
        let reset_editor = editor.clone();
        let copy_action_editor = editor.clone();
        let copy_context_editor = editor;
        let manage_button_action = action.clone();
        let manage_button_label = label.clone();
        let edit_action = action.clone();
        let edit_label = label.clone();
        let reset_action = action.clone();
        let copy_action = action;
        let context_label = category.label().to_string();

        div()
            .id(("keymap-row", index))
            .group("keymap-row")
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .border_2()
            .when(is_selected, |this| this.border_color(theme.ring))
            .when(!is_selected, |this| this.border_color(transparent_black()))
            .text_color(theme.foreground)
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.select_row(index, cx);
                    if event.click_count == 2 {
                        this.start_editing(
                            row_action.clone(),
                            row_label.clone(),
                            category,
                            window,
                            cx,
                        );
                    }
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.select_row(index, cx);
                }),
            )
            .child(
                div()
                    .w(px(ICON_COL))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(!is_selected, |this| {
                        this.opacity(0.0)
                            .group_hover("keymap-row", |style| style.opacity(1.0))
                    })
                    .child(
                        Button::new(("keymap-manage-binding", index))
                            .small()
                            .compact()
                            .ghost()
                            .icon(IconName::Pencil)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_click(move |_, window, cx| {
                                manage_button_editor.update(cx, |this, cx| {
                                    this.start_editing(
                                        manage_button_action.clone(),
                                        manage_button_label.clone(),
                                        category,
                                        window,
                                        cx,
                                    );
                                });
                                cx.stop_propagation();
                            }),
                    ),
            )
            .child(
                div()
                    .id(("keymap-action", index))
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .px_2()
                    .truncate()
                    .tooltip(text_tooltip(tooltip_text))
                    .child(cell_label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .children(if is_unbound {
                        vec![
                            div()
                                .text_size(ui_rems(12.0))
                                .text_color(theme.muted_foreground)
                                .child(crate::fl!("keymap-unbound"))
                                .into_any_element(),
                        ]
                    } else {
                        keystrokes
                            .into_iter()
                            .map(|binding| render_keybinding_sequence(binding, cx))
                            .collect::<Vec<_>>()
                    }),
            )
            .child(
                div()
                    .w(px(CONTEXT_COL))
                    .flex_none()
                    .px_2()
                    .text_size(ui_rems(12.0))
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(category.label()),
            )
            .child(
                div()
                    .w(px(SOURCE_COL))
                    .flex_none()
                    .px_2()
                    .text_size(ui_rems(12.0))
                    .text_color(theme.muted_foreground)
                    .child(source_label),
            )
            .context_menu(move |menu, _, _| {
                menu.item(
                    PopupMenuItem::new(crate::fl!("keymap-manage-keybindings"))
                        .icon(IconName::Pencil)
                        .on_click({
                            let editor = edit_editor.clone();
                            let action = edit_action.clone();
                            let label = edit_label.clone();
                            move |_, window, cx| {
                                editor.update(cx, |this, cx| {
                                    this.start_editing(
                                        action.clone(),
                                        label.clone(),
                                        category,
                                        window,
                                        cx,
                                    );
                                });
                            }
                        }),
                )
                .when(is_overridden, |menu| {
                    menu.item(
                        PopupMenuItem::new(crate::fl!("keymap-reset-to-default"))
                            .icon(IconName::RotateCcw)
                            .on_click({
                                let editor = reset_editor.clone();
                                let action = reset_action.clone();
                                move |_, _, cx| {
                                    editor.update(cx, |this, cx| {
                                        this.reset_binding(action.clone(), category, cx);
                                    });
                                }
                            }),
                    )
                })
                .separator()
                .item(
                    PopupMenuItem::new(crate::fl!("keymap-copy-action"))
                        .icon(IconName::Copy)
                        .on_click({
                            let editor = copy_action_editor.clone();
                            let action = copy_action.clone();
                            move |_, _, cx| {
                                editor.update(cx, |this, cx| {
                                    this.copy_to_clipboard(action.clone(), cx);
                                });
                            }
                        }),
                )
                .item(
                    PopupMenuItem::new(crate::fl!("keymap-copy-context"))
                        .icon(IconName::Copy)
                        .on_click({
                            let editor = copy_context_editor.clone();
                            let label = context_label.clone();
                            move |_, _, cx| {
                                editor.update(cx, |this, cx| {
                                    this.copy_to_clipboard(label.clone(), cx);
                                });
                            }
                        }),
                )
            })
            .into_any_element()
    }

    fn render_editing_content(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(editing) = self.editing.as_ref() else {
            return div().into_any_element();
        };
        let settings = &cx.global::<SettingsStore>().settings;
        let theme = cx.theme().colors;
        let radius = cx.theme().radius;
        let keys = editing
            .input
            .read(cx)
            .keystrokes()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let keystroke = keys.join(" ");
        let matching_count = self.matching_bindings_count(
            settings,
            editing.keymap,
            editing.category,
            &editing.action,
            &keystroke,
        );
        let view_keystroke = keystroke.clone();
        let input_mode = editing.input_mode;
        let editor = cx.entity();

        let draft_rows = editing
            .draft
            .iter()
            .enumerate()
            .map(|(index, binding)| {
                let replace_editor = editor.clone();
                let remove_editor = editor.clone();
                div()
                    .id(("keymap-draft-binding", index))
                    .min_h(px(40.0))
                    .w_full()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .rounded(radius)
                    .border_1()
                    .border_color(theme.input)
                    .bg(theme.popover)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(render_keybinding_sequence(binding, cx)),
                    )
                    .when(input_mode.is_none(), |this| {
                        this.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    Button::new(("keymap-replace-binding", index))
                                        .small()
                                        .ghost()
                                        .label(crate::fl!("keymap-replace-keybinding"))
                                        .icon(IconName::Pencil)
                                        .on_click(move |_, window, cx| {
                                            replace_editor.update(cx, |this, cx| {
                                                this.begin_binding_input(
                                                    BindingInputMode::Replace(index),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }),
                                )
                                .child(
                                    Button::new(("keymap-remove-binding", index))
                                        .small()
                                        .ghost()
                                        .label(crate::fl!("keymap-remove-keybinding"))
                                        .icon(IconName::X)
                                        .on_click(move |_, window, cx| {
                                            remove_editor.update(cx, |this, cx| {
                                                this.remove_draft_binding(index, window, cx);
                                            });
                                        }),
                                ),
                        )
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let input_title = match input_mode {
            Some(BindingInputMode::Add) => crate::fl!("keymap-add-keybinding"),
            Some(BindingInputMode::Replace(_)) => crate::fl!("keymap-replace-keybinding"),
            None => String::new(),
        };
        let apply_label = match input_mode {
            Some(BindingInputMode::Add) => crate::fl!("keymap-add"),
            Some(BindingInputMode::Replace(_)) => crate::fl!("keymap-update"),
            None => String::new(),
        };
        let replacing_binding = match input_mode {
            Some(BindingInputMode::Replace(index)) => editing.draft.get(index).cloned(),
            _ => None,
        };
        let add_editor = editor.clone();
        let reset_editor = editor.clone();
        let apply_editor = editor.clone();
        let cancel_input_editor = editor;

        div()
            .key_context("KeybindingManager")
            .track_focus(&editing.focus_handle)
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(ui_rems(12.0))
                    .text_color(theme.muted_foreground)
                    .child(crate::fl!(
                        "keymap-manager-subtitle",
                        action = editing.label.as_str(),
                        context = editing.category.label(),
                        keymap = editing.keymap.label()
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(ui_rems(12.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(crate::fl!("keymap-current-bindings")),
                            )
                            .when(input_mode.is_none(), |this| {
                                this.child(
                                    Button::new("keymap-reset-draft-bindings")
                                        .small()
                                        .ghost()
                                        .label(crate::fl!("keymap-reset-to-default"))
                                        .icon(IconName::RotateCcw)
                                        .on_click(move |_, window, cx| {
                                            reset_editor.update(cx, |this, cx| {
                                                this.reset_draft_bindings(window, cx);
                                            });
                                        }),
                                )
                            }),
                    )
                    .child(
                        div()
                            .max_h(px(240.0))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .overflow_y_scrollbar()
                            .when(editing.draft.is_empty(), |this| {
                                this.child(
                                    div()
                                        .px_3()
                                        .py_4()
                                        .rounded(radius)
                                        .border_1()
                                        .border_color(theme.input)
                                        .text_size(ui_rems(12.0))
                                        .text_color(theme.muted_foreground)
                                        .child(crate::fl!("keymap-no-bindings")),
                                )
                            })
                            .children(draft_rows),
                    )
                    .when(input_mode.is_none(), |this| {
                        this.child(
                            Button::new("keymap-add-draft-binding")
                                .small()
                                .outline()
                                .label(crate::fl!("keymap-add-keybinding"))
                                .icon(IconName::Plus)
                                .on_click(move |_, window, cx| {
                                    add_editor.update(cx, |this, cx| {
                                        this.begin_binding_input(BindingInputMode::Add, window, cx);
                                    });
                                }),
                        )
                    }),
            )
            .when(input_mode.is_some(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .pt_3()
                        .border_t(px(1.0))
                        .border_color(theme.border)
                        .child(
                            div()
                                .text_size(ui_rems(12.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(input_title),
                        )
                        .when_some(replacing_binding, |this, binding| {
                            this.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(ui_rems(12.0))
                                    .text_color(theme.muted_foreground)
                                    .child(crate::fl!("keymap-replacing"))
                                    .child(render_keybinding_sequence(binding, cx)),
                            )
                        })
                        .child(editing.input.clone())
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("keymap-cancel-binding-input")
                                        .small()
                                        .outline()
                                        .label(crate::fl!("cancel"))
                                        .on_click(move |_, window, cx| {
                                            cancel_input_editor.update(cx, |this, cx| {
                                                this.cancel_binding_input(window, cx);
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("keymap-apply-binding-input")
                                        .small()
                                        .primary()
                                        .label(apply_label)
                                        .on_click(move |_, window, cx| {
                                            apply_editor.update(cx, |this, cx| {
                                                this.stage_binding_input(window, cx);
                                            });
                                        }),
                                ),
                        ),
                )
            })
            .when(matching_count > 0 && !keystroke.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_size(ui_rems(12.0))
                                .text_color(theme.muted_foreground)
                                .child(crate::fl!(
                                    "keymap-conflicting-bindings",
                                    count = matching_count as i64
                                )),
                        )
                        .child(
                            Button::new("keymap-view-conflicts")
                                .small()
                                .outline()
                                .label(crate::fl!("view"))
                                .icon(IconName::ArrowUpRight)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.view_matching(view_keystroke.clone(), window, cx);
                                    window.close_dialog(cx);
                                })),
                        ),
                )
            })
            .when_some(editing.error.clone(), |this, error| {
                this.child(match error.severity {
                    InputSeverity::Warning => {
                        Alert::warning("keymap-input-warning", error.message).small()
                    }
                    InputSeverity::Error => {
                        Alert::error("keymap-input-error", error.message).small()
                    }
                })
            })
            .into_any_element()
    }
}

impl Focusable for KeymapEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for KeymapEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let settings = cx.global::<SettingsStore>().settings.clone();
        let rows = self.filtered_bindings(&settings);
        let row_count = rows.len();
        let scroll_handle = self.scroll.clone();

        div()
            .id("keymap-editor")
            .key_context("KeymapEditor")
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.tab_active)
            .text_color(theme.foreground)
            .text_size(ui_rems(13.0))
            .child(
                div()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .border_b(px(1.0))
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(ui_rems(18.0))
                            .child(crate::fl!("keymap-title")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Select::new(&self.keymap_select)
                                    .title_prefix(format!(
                                        "{} ",
                                        crate::fl!("keymap-selector-label")
                                    ))
                                    .w(px(180.0))
                                    .menu_width(px(180.0)),
                            )
                            .child(
                                div()
                                    .w(px(320.0))
                                    .child(Input::new(&self.filter_input).w_full()),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(32.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .border_b(px(1.0))
                    .border_color(theme.input)
                    .bg(theme.popover)
                    .child(self.header_cell(String::new(), None, Some(ICON_COL), theme, cx))
                    .child(self.header_cell(
                        crate::fl!("keymap-header-action"),
                        Some(SortColumn::Action),
                        None,
                        theme,
                        cx,
                    ))
                    .child(self.header_cell(
                        crate::fl!("keymap-header-keystrokes"),
                        Some(SortColumn::Keystrokes),
                        None,
                        theme,
                        cx,
                    ))
                    .child(self.header_cell(
                        crate::fl!("keymap-header-context"),
                        Some(SortColumn::Category),
                        Some(CONTEXT_COL),
                        theme,
                        cx,
                    ))
                    .child(self.header_cell(
                        crate::fl!("keymap-header-source"),
                        None,
                        Some(SOURCE_COL),
                        theme,
                        cx,
                    )),
            )
            .child(
                div()
                    .id("keymap-editor-table")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .relative()
                            .size_full()
                            .child(
                                uniform_list(
                                    "keymap-editor-list",
                                    row_count,
                                    cx.processor(
                                        move |this: &mut KeymapEditor,
                                              visible_range: Range<usize>,
                                              _window,
                                              cx| {
                                            visible_range
                                                .filter_map(|ix| {
                                                    rows.get(ix)
                                                        .cloned()
                                                        .map(|binding| (ix, binding))
                                                })
                                                .map(|(ix, binding)| {
                                                    this.render_row(ix, binding, theme, cx)
                                                })
                                                .collect::<Vec<_>>()
                                        },
                                    ),
                                )
                                .track_scroll(&scroll_handle)
                                .size_full(),
                            )
                            .vertical_scrollbar_for_with_id(
                                &scroll_handle,
                                "keymap-editor-scrollbar",
                                window,
                                cx,
                            )
                            .when(row_count == 0, |this| {
                                this.child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(theme.muted_foreground)
                                        .child(crate::fl!("keymap-empty")),
                                )
                            }),
                    ),
            )
            .when_some(self.save_error.clone(), |this, error| {
                this.child(
                    Alert::error("keymap-save-error", error)
                        .small()
                        .absolute()
                        .left(px(16.0))
                        .bottom(px(16.0))
                        .w(px(420.0)),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_stroke_bindings_format_as_single_sequence_labels() {
        if cfg!(target_os = "macos") {
            assert_eq!(format_keybinding_sequence("ctrl-w o"), "⌃W O");
            assert_eq!(format_keybinding_sequence("ctrl-w ctrl-o"), "⌃W ⌃O");
        } else {
            assert_eq!(format_keybinding_sequence("ctrl-w o"), "Ctrl+W O");
            assert_eq!(format_keybinding_sequence("ctrl-w ctrl-o"), "Ctrl+W Ctrl+O");
        }
    }

    #[test]
    fn draft_add_and_replace_preserve_binding_order() {
        let mut draft = vec!["ctrl-a".to_string(), "ctrl-b".to_string()];

        stage_draft_binding(
            &mut draft,
            BindingInputMode::Replace(0),
            "ctrl-c".to_string(),
        )
        .unwrap();
        stage_draft_binding(&mut draft, BindingInputMode::Add, "ctrl-d".to_string()).unwrap();

        assert_eq!(draft, ["ctrl-c", "ctrl-b", "ctrl-d"]);
    }

    #[test]
    fn draft_rejects_duplicates_except_the_binding_being_replaced() {
        let mut draft = vec!["ctrl-a".to_string(), "ctrl-b".to_string()];

        assert_eq!(
            stage_draft_binding(&mut draft, BindingInputMode::Add, "ctrl-a".to_string(),),
            Err(DraftBindingError::Duplicate)
        );
        assert_eq!(
            stage_draft_binding(
                &mut draft,
                BindingInputMode::Replace(1),
                "ctrl-a".to_string(),
            ),
            Err(DraftBindingError::Duplicate)
        );
        assert_eq!(
            stage_draft_binding(
                &mut draft,
                BindingInputMode::Replace(1),
                "ctrl-b".to_string(),
            ),
            Ok(())
        );
    }
}
