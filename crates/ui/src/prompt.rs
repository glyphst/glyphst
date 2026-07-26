use crate::menu;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, IntoElement, PromptButton, PromptHandle,
    PromptLevel, PromptResponse, Render, RenderablePromptHandle, Window, div, prelude::*, px,
};
use gpui_component::{
    Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    dialog::{AlertDialog, DialogFooter},
    scroll::ScrollableElement as _,
};

pub fn init(cx: &mut App) {
    cx.set_prompt_builder(glyphst_prompt_renderer);
}

fn glyphst_prompt_renderer(
    level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    actions: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let renderer = cx.new(|cx| GlyphstPrompt {
        level,
        message: message.into(),
        detail: detail.filter(|detail| !detail.is_empty()).map(Into::into),
        actions: actions.to_vec(),
        active_action: default_action(level, actions),
        focus_handle: cx.focus_handle(),
    });
    handle.with_view(renderer, window, cx)
}

fn default_action(level: PromptLevel, actions: &[PromptButton]) -> usize {
    if matches!(level, PromptLevel::Warning | PromptLevel::Critical)
        && let Some(index) = actions.iter().position(PromptButton::is_cancel)
    {
        return index;
    }
    actions
        .iter()
        .position(|action| !action.is_cancel())
        .unwrap_or(0)
}

struct GlyphstPrompt {
    level: PromptLevel,
    message: gpui::SharedString,
    detail: Option<gpui::SharedString>,
    actions: Vec<PromptButton>,
    active_action: usize,
    focus_handle: FocusHandle,
}

impl GlyphstPrompt {
    fn confirm(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(PromptResponse(self.active_action));
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.actions.iter().position(PromptButton::is_cancel) {
            cx.emit(PromptResponse(index));
        }
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if !self.actions.is_empty() {
            self.active_action = (self.active_action + 1) % self.actions.len();
            cx.notify();
        }
    }

    fn select_prev(&mut self, _: &menu::SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        if !self.actions.is_empty() {
            self.active_action = self
                .active_action
                .checked_sub(1)
                .unwrap_or(self.actions.len() - 1);
            cx.notify();
        }
    }
}

impl Render for GlyphstPrompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let buttons = self
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let selected = index == self.active_action;
                let destructive = !action.is_cancel()
                    && matches!(self.level, PromptLevel::Warning | PromptLevel::Critical);
                Button::new(("glyphst-prompt-action", index))
                    .small()
                    .label(action.label().clone())
                    .min_w(px(76.0))
                    .selected(selected)
                    .when(destructive, |this| this.danger())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(PromptResponse(index));
                        cx.stop_propagation();
                    }))
            })
            .collect::<Vec<_>>();
        let detail = self.detail.clone();

        div()
            .key_context("Menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_prev))
            .size_full()
            .child(
                AlertDialog::new(cx)
                    .width(px(480.0))
                    .keyboard(false)
                    .title(self.message.clone())
                    .when_some(detail, |dialog, detail| {
                        dialog.description(
                            div()
                                .max_h(px(240.0))
                                .whitespace_normal()
                                .overflow_y_scrollbar()
                                .child(detail),
                        )
                    })
                    .footer(DialogFooter::new().flex_wrap().children(buttons)),
            )
    }
}

impl EventEmitter<PromptResponse> for GlyphstPrompt {}

impl Focusable for GlyphstPrompt {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_prompts_default_to_cancel() {
        let actions = [PromptButton::ok("Delete"), PromptButton::cancel("Cancel")];
        assert_eq!(default_action(PromptLevel::Warning, &actions), 1);
        assert_eq!(default_action(PromptLevel::Info, &actions), 0);
    }
}
