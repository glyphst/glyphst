use crate::{library::HighlightColor, settings::SettingsStore};
pub use glyphst_actions::note_editor::NoteCancel;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton,
    PromptButton, PromptLevel, Render, SharedString, Window, div, prelude::*, px, rgba,
};
use gpui_component::{
    ActiveTheme as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogFooter, DialogHeader, DialogTitle},
    input::{Input, InputState},
};

pub enum NoteEditorEvent {
    Save {
        body: SharedString,
        color: HighlightColor,
    },
    Cancel,
}

/// Domain wrapper around gpui-component's multi-line input.
pub struct NoteEditor {
    previous_focus_handle: FocusHandle,
    input: Entity<InputState>,
    color: HighlightColor,
    initial_body: String,
    initial_color: HighlightColor,
    discard_prompt_pending: bool,
}

impl NoteEditor {
    pub fn new(
        previous_focus_handle: FocusHandle,
        content: SharedString,
        color: HighlightColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let content_len = content.len();
        let initial_body = content.trim().to_string();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(8)
                .soft_wrap(true)
                .placeholder("Write a note…")
                .default_value(content)
        });
        input.update(cx, |input, cx| {
            input.set_selected_range(0..content_len, cx);
        });

        Self {
            previous_focus_handle,
            input,
            color,
            initial_body,
            initial_color: color,
            discard_prompt_pending: false,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus_handle.focus(window, cx);
        cx.emit(NoteEditorEvent::Save {
            body: self.input.read(cx).value(),
            color: self.color,
        });
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus_handle.focus(window, cx);
        cx.emit(NoteEditorEvent::Cancel);
    }

    fn has_unsaved_changes(&self, cx: &App) -> bool {
        self.input.read(cx).value().trim() != self.initial_body || self.color != self.initial_color
    }

    pub fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.discard_prompt_pending {
            return;
        }
        if !self.has_unsaved_changes(cx) {
            self.dismiss(window, cx);
            return;
        }

        let title = crate::fl!("note-discard-title");
        let detail = crate::fl!("note-discard-detail");
        let confirmation = window.prompt(
            PromptLevel::Warning,
            &title,
            Some(&detail),
            &[
                PromptButton::ok(crate::fl!("discard")),
                PromptButton::cancel(crate::fl!("keep-editing")),
            ],
            cx,
        );
        self.discard_prompt_pending = true;
        cx.spawn_in(window, async move |this, cx| {
            let discard = confirmation.await == Ok(0);
            _ = this.update_in(cx, |this, window, cx| {
                this.discard_prompt_pending = false;
                if discard {
                    this.dismiss(window, cx);
                } else {
                    this.input.focus_handle(cx).focus(window, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn cancel(&mut self, _: &NoteCancel, window: &mut Window, cx: &mut Context<Self>) {
        self.request_close(window, cx);
    }

    fn set_color(&mut self, color: HighlightColor, cx: &mut Context<Self>) {
        if self.color != color {
            self.color = color;
            cx.notify();
        }
    }
}

impl EventEmitter<NoteEditorEvent> for NoteEditor {}

impl Focusable for NoteEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for NoteEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let document_theme = cx.global::<SettingsStore>().resolved_theme;
        let selected_color = self.color;
        let mut color_palette = div().flex().flex_row().gap_2();
        for color in HighlightColor::ALL {
            color_palette = color_palette.child(
                div()
                    .id(format!("note-editor-color-{color:?}"))
                    .w(px(22.0))
                    .h(px(22.0))
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(if selected_color == color {
                        cx.theme().ring
                    } else {
                        cx.theme().border
                    })
                    .bg(rgba(document_theme.note_color(color)))
                    .cursor_pointer()
                    .hover(|style| style.border_color(cx.theme().ring))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.set_color(color, cx);
                            cx.stop_propagation();
                        }),
                    ),
            );
        }

        div()
            .key_context("NoteEditor")
            .flex()
            .flex_col()
            .w(px(560.0))
            .popover_style(cx)
            .p_0()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_action(cx.listener(Self::cancel))
            .child(
                DialogHeader::new()
                    .h(px(42.0))
                    .flex_none()
                    .justify_center()
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(DialogTitle::new().child(crate::fl!("note-title"))),
            )
            .child(
                Input::new(&self.input)
                    .h(px(190.0))
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false)
                    .p_3(),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(40.0))
                    .flex_none()
                    .px_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(color_palette)
                    .child(
                        DialogFooter::new()
                            .child(
                                Button::new("note-editor-close")
                                    .label(crate::fl!("close"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.request_close(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("note-editor-save")
                                    .label(crate::fl!("save"))
                                    .primary()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save(window, cx);
                                    })),
                            ),
                    ),
            )
    }
}
