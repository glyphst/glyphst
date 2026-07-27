use std::{cmp::Ordering, ops::Range};

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, Focusable, IntoElement,
    Keystroke, MouseButton, MouseDownEvent, ParentElement, Render, SharedString, Styled,
    UniformListScrollHandle, Window, div, prelude::*, px, transparent_black, uniform_list,
};

use crate::{
    icons::{Icon, IconName},
    keybindings::{self, EditableKeyBinding, KeyBindingCategory},
    keystroke_recorder::{KeystrokeRecorder, KeystrokeRecorderEvent},
    scrollbar::WithScrollbar as _,
    settings::{Settings, SettingsStore, ui_rems},
};
use glyphst_ui::tooltip::text_tooltip;
use gpui_component::{
    ActiveTheme as _, Sizable as _, ThemeColor as Theme, WindowExt as _,
    alert::Alert,
    button::Button,
    dialog::DialogButtonProps,
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    menu::{ContextMenuExt as _, PopupMenuItem},
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

fn render_keybinding_sequence(sequence: impl AsRef<str>) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .children(sequence.as_ref().split_whitespace().map(|stroke| {
            Keystroke::parse(stroke)
                .map(|stroke| Kbd::new(stroke).outline().into_any_element())
                .unwrap_or_else(|_| div().child(stroke.to_string()).into_any_element())
        }))
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

struct EditModal {
    action: String,
    label: String,
    category: KeyBindingCategory,
    append: bool,
    input: Entity<KeystrokeRecorder>,
    error: Option<InputError>,
}

pub struct KeymapEditor {
    focus_handle: FocusHandle,
    filter_input: Entity<InputState>,
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
        let filter_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::fl!("keymap-search-placeholder"))
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
        cx.observe_global::<SettingsStore>(|_, cx| cx.notify())
            .detach();

        Self {
            focus_handle,
            filter_input,
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

    fn filtered_bindings(&self, settings: &Settings) -> Vec<EditableKeyBinding> {
        let query = self.query.trim().to_ascii_lowercase();
        let mut bindings =
            keybindings::editable_bindings(settings.keymap_preset, &settings.keybinding_overrides)
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
        append: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(KeystrokeRecorder::new);
        cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
            KeystrokeRecorderEvent::Changed(keys) => {
                let _ = keys.len();
                cx.notify();
            }
            KeystrokeRecorderEvent::Cancel => {
                this.cancel_editing(cx);
                window.close_dialog(cx);
            }
        })
        .detach();
        let title = if append {
            crate::fl!("keymap-add-keybinding")
        } else {
            crate::fl!("keymap-edit-keystroke")
        };
        self.editing = Some(EditModal {
            action,
            label,
            category,
            append,
            input: input.clone(),
            error: None,
        });

        let editor = cx.entity();
        let content_editor = editor.clone();
        let save_editor = editor.clone();
        let close_editor = editor;
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .width(px(544.0))
                .title(title.clone())
                .content({
                    let editor = content_editor.clone();
                    move |content, window, cx| {
                        let body =
                            editor.update(cx, |this, cx| this.render_editing_content(window, cx));
                        content.child(body)
                    }
                })
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(crate::fl!("save"))
                        .cancel_text(crate::fl!("cancel"))
                        .show_cancel(true)
                        .on_ok({
                            let editor = save_editor.clone();
                            move |_, _, cx| editor.update(cx, |this, cx| this.save_editing(cx))
                        }),
                )
                .on_close({
                    let editor = close_editor.clone();
                    move |_, _, cx| {
                        editor.update(cx, |this, cx| this.cancel_editing(cx));
                    }
                })
        });
        let input_to_focus = input;
        window.defer(cx, move |window, cx| {
            input_to_focus.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    fn cancel_editing(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        cx.notify();
    }

    fn save_editing(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(editing) = &self.editing else {
            return true;
        };
        let action = editing.action.clone();
        let category = editing.category;
        let append = editing.append;
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

        let mut settings = cx.global::<SettingsStore>().settings.clone();
        let mut keystrokes = if append {
            keybindings::editable_bindings(settings.keymap_preset, &settings.keybinding_overrides)
                .into_iter()
                .find(|binding| binding.action == action && binding.category == category)
                .map(|binding| binding.keystrokes)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        keystrokes.push(keystroke);
        keystrokes.sort();
        keystrokes.dedup();

        keybindings::set_keybinding_overrides(&mut settings, action, category, keystrokes);
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

    fn clear_binding(
        &mut self,
        action: String,
        category: KeyBindingCategory,
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        keybindings::set_keybinding_overrides(&mut settings, action, category, Vec::new());
        self.persist_settings(settings, cx);
    }

    fn reset_binding(
        &mut self,
        action: String,
        category: KeyBindingCategory,
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        keybindings::reset_keybinding_overrides(&mut settings, &action, category);
        self.persist_settings(settings, cx);
    }

    fn matching_bindings_count(
        &self,
        settings: &Settings,
        category: KeyBindingCategory,
        action: &str,
        keystroke: &str,
    ) -> usize {
        if keystroke.is_empty() {
            return 0;
        }

        keybindings::editable_bindings(settings.keymap_preset, &settings.keybinding_overrides)
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
        let edit_editor = editor.clone();
        let reset_editor = editor.clone();
        let delete_editor = editor.clone();
        let copy_action_editor = editor.clone();
        let copy_context_editor = editor;
        let edit_action = action.clone();
        let edit_label = label.clone();
        let reset_action = action.clone();
        let delete_action = action.clone();
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
                            false,
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
                    .when(!is_overridden, |this| {
                        this.opacity(0.0)
                            .group_hover("keymap-row", |style| style.opacity(1.0))
                    })
                    .child(
                        Icon::new(if is_overridden {
                            IconName::Info
                        } else {
                            IconName::Pencil
                        })
                        .size(ui_rems(14.0))
                        .text_color(theme.muted_foreground),
                    ),
            )
            .child(
                div()
                    .id(("keymap-action", index))
                    .flex_1()
                    .min_w_0()
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
                            .map(render_keybinding_sequence)
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
                    PopupMenuItem::new(crate::fl!("edit"))
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
                                        false,
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
                .when(!is_unbound, |menu| {
                    menu.item(
                        PopupMenuItem::new(crate::fl!("delete"))
                            .icon(IconName::Delete)
                            .on_click({
                                let editor = delete_editor.clone();
                                let action = delete_action.clone();
                                move |_, _, cx| {
                                    editor.update(cx, |this, cx| {
                                        this.clear_binding(action.clone(), category, cx);
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
        let keys = editing
            .input
            .read(cx)
            .keystrokes()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let keystroke = keys.join(" ");
        let matching_count =
            self.matching_bindings_count(settings, editing.category, &editing.action, &keystroke);
        let view_keystroke = keystroke.clone();

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(ui_rems(12.0))
                    .text_color(theme.muted_foreground)
                    .child(crate::fl!(
                        "keymap-modal-subtitle",
                        action = editing.label.as_str(),
                        context = editing.category.label()
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui_rems(12.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(crate::fl!("keymap-edit-keystroke")),
                    )
                    .child(editing.input.clone()),
            )
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
            .child(
                div()
                    .text_size(ui_rems(12.0))
                    .text_color(theme.muted_foreground)
                    .child(crate::fl!(
                        "keymap-context",
                        context = editing.category.label()
                    )),
            )
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
                            .w(px(300.0))
                            .child(Input::new(&self.filter_input).small().w_full()),
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
                            ),
                    ),
            )
            .when(row_count == 0, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(116.0))
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.muted_foreground)
                        .child(crate::fl!("keymap-empty")),
                )
            })
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
