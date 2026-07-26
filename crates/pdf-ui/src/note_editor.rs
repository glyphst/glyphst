use crate::{
    library::HighlightColor,
    settings::{SettingsStore, ui_rems},
};
pub use glyphst_actions::note_editor::{NoteCancel, NoteSave};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, Render,
    SharedString, Window, div, prelude::*, px, rgba,
};
use gpui_component::{
    ActiveTheme as _, StyledExt as _,
    dialog::{DialogHeader, DialogTitle},
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
        }
    }

    fn save(&mut self, _: &NoteSave, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus_handle.focus(window, cx);
        cx.emit(NoteEditorEvent::Save {
            body: self.input.read(cx).value(),
            color: self.color,
        });
    }

    fn cancel(&mut self, _: &NoteCancel, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus_handle.focus(window, cx);
        cx.emit(NoteEditorEvent::Cancel);
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
            .on_action(cx.listener(Self::save))
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
                    .text_size(ui_rems(12.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(color_palette)
                    .child(crate::fl!("note-editor-help")),
            )
    }
}
