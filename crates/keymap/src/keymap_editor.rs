use std::{cmp::Ordering, ops::Range};

use gpui::{
    Anchor, AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render, SharedString, Styled,
    UniformListScrollHandle, Window, anchored, deferred, div, prelude::*, px, rems, rgb, rgba,
    uniform_list,
};

use crate::{
    icons::{Icon, IconName},
    keybindings::{self, EditableKeyBinding, KeyBindingCategory},
    keystroke_input::{
        KeystrokeInput, KeystrokeInputEvent, format_keystroke, render_keybinding_keystroke,
    },
    scrollbar::uniform_list_scrollbar_for,
    settings::{Settings, SettingsStore, ui_rems},
    theme::Theme,
};
use glyphst_ui::tooltip::text_tooltip;
use gpui_component::{
    Sizable as _,
    input::{Input, InputEvent, InputState},
};

const ROW_HEIGHT: f32 = 36.0;
const ICON_COL: f32 = 36.0;
const CONTEXT_COL: f32 = 170.0;
const SOURCE_COL: f32 = 90.0;

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
    input: Entity<KeystrokeInput>,
    error: Option<InputError>,
}

#[derive(Clone)]
struct ContextMenu {
    position: Point<Pixels>,
    action: String,
    label: String,
    category: KeyBindingCategory,
    is_overridden: bool,
    is_unbound: bool,
}

pub struct KeymapEditor {
    focus_handle: FocusHandle,
    filter_input: Entity<InputState>,
    query: SharedString,
    scroll: UniformListScrollHandle,
    sort_column: SortColumn,
    sort_ascending: bool,
    selected_index: Option<usize>,
    context_menu: Option<ContextMenu>,
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
            context_menu: None,
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

    fn open_context_menu(&mut self, menu: ContextMenu, cx: &mut Context<Self>) {
        self.context_menu = Some(menu);
        cx.notify();
    }

    fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
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
        let input = cx.new(KeystrokeInput::new);
        cx.subscribe(&input, |this, _, event, cx| match event {
            KeystrokeInputEvent::Changed(keys) => {
                let _ = keys.len();
                cx.notify();
            }
            KeystrokeInputEvent::Cancel => this.cancel_editing(cx),
        })
        .detach();
        input.update(cx, |input, cx| input.focus(window, cx));
        self.editing = Some(EditModal {
            action,
            label,
            category,
            append,
            input,
            error: None,
        });
        cx.notify();
    }

    fn cancel_editing(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        cx.notify();
    }

    fn save_editing(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = &self.editing else {
            return;
        };
        let action = editing.action.clone();
        let category = editing.category;
        let append = editing.append;
        let keys = editing.input.read(cx).keystrokes().to_vec();

        if keys.is_empty() {
            self.set_input_error(
                InputSeverity::Warning,
                crate::fl!("keymap-error-press-shortcut").into(),
                cx,
            );
            return;
        }

        let keystroke = keys.join(" ");
        if let Err(error) = keybindings::validate_keybinding(cx, &action, category, &keystroke) {
            self.set_input_error(InputSeverity::Error, error.into(), cx);
            return;
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
            .text_color(rgb(theme.text_muted));
        let base = match width {
            Some(width) => base.w(px(width)).flex_none(),
            None => base.flex_1().min_w_0(),
        };

        match column {
            Some(column) => base
                .id(format!("keymap-column-{column:?}"))
                .hover(move |style| style.bg(rgb(theme.element_hover)))
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
                .map(|key| format_keystroke(key).to_string())
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

        div()
            .id(("keymap-row", index))
            .group("keymap-row")
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .border_2()
            .when(is_selected, |this| {
                this.border_color(rgb(theme.border_focused))
            })
            .when(!is_selected, |this| this.border_color(rgba(0x00000000)))
            .text_color(rgb(theme.text))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
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
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.select_row(index, cx);
                    this.open_context_menu(
                        ContextMenu {
                            position: event.position,
                            action: action.clone(),
                            label: label.clone(),
                            category,
                            is_overridden,
                            is_unbound,
                        },
                        cx,
                    );
                    cx.stop_propagation();
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
                        .text_color(rgb(theme.text_muted)),
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
                                .text_color(rgb(theme.text_muted))
                                .child(crate::fl!("keymap-unbound"))
                                .into_any_element(),
                        ]
                    } else {
                        keystrokes
                            .into_iter()
                            .map(|key| render_keybinding_keystroke(theme, key))
                            .collect::<Vec<_>>()
                    }),
            )
            .child(
                div()
                    .w(px(CONTEXT_COL))
                    .flex_none()
                    .px_2()
                    .text_size(ui_rems(12.0))
                    .text_color(rgb(theme.text_muted))
                    .truncate()
                    .child(category.label()),
            )
            .child(
                div()
                    .w(px(SOURCE_COL))
                    .flex_none()
                    .px_2()
                    .text_size(ui_rems(12.0))
                    .text_color(rgb(theme.text_muted))
                    .child(source_label),
            )
            .into_any_element()
    }

    fn modal_button(
        &self,
        label: String,
        primary: bool,
        theme: Theme,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(label.clone())
            .h(px(28.0))
            .px_3()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(if primary {
                theme.accent
            } else {
                theme.surface_background
            }))
            .text_size(ui_rems(12.0))
            .text_color(rgb(if primary {
                theme.background
            } else {
                theme.text
            }))
            .hover(move |style| {
                style.bg(rgb(if primary {
                    theme.accent
                } else {
                    theme.element_hover
                }))
            })
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    on_click(this, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
            .into_any_element()
    }

    fn menu_entry(
        label: String,
        icon: IconName,
        theme: Theme,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(label.clone())
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.0))
            .px_3()
            .text_size(ui_rems(13.0))
            .text_color(rgb(theme.text))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    handler(this, window, cx);
                    this.close_context_menu(cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                Icon::new(icon)
                    .size(ui_rems(14.0))
                    .text_color(rgb(theme.text_muted)),
            )
            .child(label)
            .into_any_element()
    }

    fn menu_separator(theme: Theme) -> AnyElement {
        div()
            .h(px(1.0))
            .my_1()
            .mx_1()
            .bg(rgb(theme.border))
            .into_any_element()
    }

    fn render_context_menu(menu: &ContextMenu, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let action = menu.action.clone();
        let label = menu.label.clone();
        let category = menu.category;
        let is_overridden = menu.is_overridden;
        let is_unbound = menu.is_unbound;
        let edit_action = action.clone();
        let reset_action = action.clone();
        let delete_action = action.clone();
        let copy_action = action;

        div()
            .occlude()
            .flex()
            .flex_col()
            .w(px(220.0))
            .py_1()
            .border_1()
            .border_color(rgb(theme.border))
            .overflow_hidden()
            .bg(rgb(theme.surface_background))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Right, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .child(Self::menu_entry(
                crate::fl!("edit"),
                IconName::Pencil,
                theme,
                move |this, window, cx| {
                    this.start_editing(
                        edit_action.clone(),
                        label.clone(),
                        category,
                        false,
                        window,
                        cx,
                    );
                },
                cx,
            ))
            .when(is_overridden, |menu| {
                menu.child(Self::menu_entry(
                    crate::fl!("keymap-reset-to-default"),
                    IconName::RotateCcw,
                    theme,
                    move |this, _, cx| {
                        this.reset_binding(reset_action.clone(), category, cx);
                    },
                    cx,
                ))
            })
            .when(!is_unbound, |menu| {
                menu.child(Self::menu_entry(
                    crate::fl!("delete"),
                    IconName::Delete,
                    theme,
                    move |this, _, cx| {
                        this.clear_binding(delete_action.clone(), category, cx);
                    },
                    cx,
                ))
            })
            .child(Self::menu_separator(theme))
            .child(Self::menu_entry(
                crate::fl!("keymap-copy-action"),
                IconName::Copy,
                theme,
                move |this, _, cx| {
                    this.copy_to_clipboard(copy_action.clone(), cx);
                },
                cx,
            ))
            .child(Self::menu_entry(
                crate::fl!("keymap-copy-context"),
                IconName::Copy,
                theme,
                move |this, _, cx| {
                    this.copy_to_clipboard(category.label().to_string(), cx);
                },
                cx,
            ))
            .into_any_element()
    }

    // __KM_MENU_RENDER__

    fn render_modal(
        &self,
        editing: &EditModal,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let keys = editing.input.read(cx).keystrokes().to_vec();
        let keystroke = keys.join(" ");
        let matching_count =
            self.matching_bindings_count(settings, editing.category, &editing.action, &keystroke);
        let title = if editing.append {
            crate::fl!("keymap-add-keybinding")
        } else {
            crate::fl!("keymap-edit-keystroke")
        };

        let view_keystroke = keystroke.clone();

        div()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000066))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.cancel_editing(cx);
                }),
            )
            .child(
                div()
                    .w(rems(34.0))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(theme.border_variant))
                    .bg(rgb(theme.surface_background))
                    .shadow_lg()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .pb_3()
                            .border_b_1()
                            .border_color(rgb(theme.border))
                            .child(
                                div()
                                    .text_size(ui_rems(16.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(ui_rems(12.0))
                                    .text_color(rgb(theme.text_muted))
                                    .child(crate::fl!(
                                        "keymap-modal-subtitle",
                                        action = editing.label.as_str(),
                                        context = editing.category.label()
                                    )),
                            ),
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
                                        .text_color(rgb(theme.text_muted))
                                        .child(crate::fl!(
                                            "keymap-conflicting-bindings",
                                            count = matching_count as i64
                                        )),
                                )
                                .child(
                                    div()
                                        .id("keymap-view-conflicts")
                                        .px_2()
                                        .py_1()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_1()
                                        .rounded(px(4.0))
                                        .border_1()
                                        .border_color(rgb(theme.border_variant))
                                        .text_size(ui_rems(11.0))
                                        .text_color(rgb(theme.text_muted))
                                        .hover(|style| style.bg(rgb(theme.element_hover)))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, window, cx| {
                                                this.view_matching(
                                                    view_keystroke.clone(),
                                                    window,
                                                    cx,
                                                );
                                                cx.stop_propagation();
                                            }),
                                        )
                                        .child(crate::fl!("view"))
                                        .child(
                                            Icon::new(IconName::ArrowUpRight)
                                                .size(ui_rems(11.0))
                                                .text_color(rgb(theme.text_muted)),
                                        ),
                                ),
                        )
                    })
                    .child(
                        div()
                            .text_size(ui_rems(12.0))
                            .text_color(rgb(theme.text_muted))
                            .child(crate::fl!(
                                "keymap-context",
                                context = editing.category.label()
                            )),
                    )
                    .when_some(editing.error.clone(), |this, error| {
                        let (bg_color, border_color, text_color) = match error.severity {
                            InputSeverity::Warning => {
                                let warn = rgb(theme.error).opacity(0.7);
                                (warn.opacity(0.1), warn, warn)
                            }
                            InputSeverity::Error => {
                                let err = rgb(theme.error);
                                (err.opacity(0.1), err, err)
                            }
                        };
                        this.child(
                            div()
                                .px_3()
                                .py_2()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(border_color)
                                .bg(bg_color)
                                .text_color(text_color)
                                .text_size(ui_rems(12.0))
                                .child(
                                    Icon::new(IconName::TriangleAlert)
                                        .size(ui_rems(14.0))
                                        .text_color(text_color),
                                )
                                .child(error.message),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap_2()
                            .child(self.modal_button(
                                crate::fl!("cancel"),
                                false,
                                theme,
                                |this, _, cx| {
                                    this.cancel_editing(cx);
                                },
                                cx,
                            ))
                            .child(self.modal_button(
                                crate::fl!("save"),
                                true,
                                theme,
                                |this, _, cx| {
                                    this.save_editing(cx);
                                },
                                cx,
                            )),
                    ),
            )
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
        let (settings, theme) = {
            let store = cx.global::<SettingsStore>();
            (store.settings.clone(), store.resolved_theme)
        };
        let rows = self.filtered_bindings(&settings);
        let row_count = rows.len();
        let scroll_handle = self.scroll.clone();
        let editing = self.editing.as_ref();
        let context_menu = self.context_menu.as_ref();

        div()
            .id("keymap-editor")
            .key_context("KeymapEditor")
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.tab_active_background))
            .text_color(rgb(theme.text))
            .text_size(ui_rems(13.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_context_menu(cx);
                }),
            )
            .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                this.close_context_menu(cx);
            }))
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
                    .border_color(rgb(theme.border))
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
                    .border_color(rgb(theme.border_variant))
                    .bg(rgb(theme.surface_background))
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
                                            rows.get(ix).cloned().map(|binding| (ix, binding))
                                        })
                                        .map(|(ix, binding)| {
                                            this.render_row(ix, binding, theme, cx)
                                        })
                                        .collect::<Vec<_>>()
                                },
                            ),
                        )
                        .track_scroll(&scroll_handle)
                        .with_decoration(uniform_list_scrollbar_for(
                            &scroll_handle,
                            "keymap-editor-scrollbar",
                            window,
                            cx,
                        ))
                        .size_full(),
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
                        .text_color(rgb(theme.text_muted))
                        .child(crate::fl!("keymap-empty")),
                )
            })
            .when_some(context_menu, |this, menu| {
                this.child(
                    deferred(
                        anchored()
                            .snap_to_window_with_margin(px(8.0))
                            .position(menu.position)
                            .anchor(Anchor::TopLeft)
                            .child(Self::render_context_menu(menu, theme, cx)),
                    )
                    .with_priority(1),
                )
            })
            .when_some(self.save_error.clone(), |this, error| {
                this.child(
                    div()
                        .absolute()
                        .left(px(16.0))
                        .bottom(px(16.0))
                        .px_3()
                        .py_2()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(rgb(theme.error))
                        .bg(rgb(theme.surface_background))
                        .text_color(rgb(theme.error))
                        .child(error),
                )
            })
            .when_some(editing, |this, editing| {
                this.child(self.render_modal(editing, &settings, theme, cx))
            })
    }
}
