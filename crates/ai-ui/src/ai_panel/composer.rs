use super::helpers::*;
use super::*;

fn changed_range(old: &str, new: &str) -> (Range<usize>, usize) {
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(old, new)| old == new)
        .count();
    while prefix > 0 && (!old.is_char_boundary(prefix) || !new.is_char_boundary(prefix)) {
        prefix -= 1;
    }

    let old_tail = &old[prefix..];
    let new_tail = &new[prefix..];
    let mut suffix = old_tail
        .bytes()
        .rev()
        .zip(new_tail.bytes().rev())
        .take_while(|(old, new)| old == new)
        .count()
        .min(old_tail.len())
        .min(new_tail.len());
    while suffix > 0
        && (!old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix))
    {
        suffix -= 1;
    }

    (prefix..old.len() - suffix, new.len() - prefix - suffix)
}

impl AiPanel {
    pub(super) fn on_composer_input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let (new_content, selected_range) = {
                    let input = self.composer_input.read(cx);
                    (input.value(), input.selected_range())
                };

                let (range, inserted_len) = changed_range(&self.content, &new_content);
                if range.start != range.end || inserted_len != 0 {
                    adjust_composer_mentions(&mut self.composer_mentions, &range, inserted_len);
                }
                self.content = new_content;
                self.selected_range = selected_range;
                self.selection_reversed = false;
                self.marked_range = None;
                self.update_mention_completion(cx);
                self.clear_validation_notice();
                cx.notify();
            }
            InputEvent::PressEnter { shift, .. } if !shift => {
                if self.confirm_mention_completion(cx) {
                    self.sync_composer_input(window, cx);
                    return;
                }
                match submit_action(
                    self.editing_turn.is_some(),
                    self.generation_failure.as_ref(),
                ) {
                    SubmitAction::Edit => self.submit_edited_message(window, cx),
                    SubmitAction::Retry(assistant_index) => {
                        self.retry_assistant(assistant_index, cx)
                    }
                    SubmitAction::Send => self.send_message(cx),
                }
                self.sync_composer_input(window, cx);
            }
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {
                cx.notify();
            }
        }
    }

    pub(super) fn on_model_search_input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.model_search_query = self.model_search_input.read(cx).value();
            cx.notify();
        }
    }

    pub(super) fn sync_composer_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.composer_input.read(cx).value();
        if value != self.content {
            let content = self.content.clone();
            let selected_range = self.selected_range.clone();
            self.composer_input.update(cx, |input, cx| {
                input.set_value(content, window, cx);
                input.set_selected_range(selected_range, cx);
            });
        }
    }

    pub(super) fn sync_model_search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.model_search_input.read(cx).value();
        if value != self.model_search_query {
            let query = self.model_search_query.clone();
            self.model_search_input.update(cx, |input, cx| {
                input.set_value(query, window, cx);
            });
        }
    }

    pub(super) fn composer_snapshot(&self, cx: &App) -> ComposerSnapshot {
        let input = self.composer_input.read(cx);
        ComposerSnapshot {
            content: self.content.clone(),
            selected_range: input.selected_range(),
            selection_reversed: false,
            marked_range: None,
            mentions: self.composer_mentions.clone(),
        }
    }

    pub(super) fn restore_composer_snapshot(
        &mut self,
        snapshot: ComposerSnapshot,
        cx: &mut Context<Self>,
    ) {
        self.content = snapshot.content;
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = snapshot.selection_reversed;
        self.marked_range = snapshot.marked_range;
        self.composer_mentions = snapshot.mentions;
        self.mention_completion = None;
        self.update_mention_completion(cx);
        cx.notify();
    }

    pub(super) fn start_editing_message(
        &mut self,
        message_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading || self.editing_turn.is_some() || self.mode != AiPanelMode::Sidebar {
            return;
        }
        let Some(message) = self.messages.get(message_index) else {
            return;
        };
        if message.role != Role::User {
            return;
        }

        let content = message.display_text();
        let Ok(mentions) = composer_mentions_for_message(message) else {
            self.show_validation_notice(tr("ai-edit-context-unavailable"), cx);
            return;
        };
        self.editing_turn = Some(EditingTurn {
            message_index,
            saved_composer: self.composer_snapshot(cx),
        });
        self.content = content.clone().into();
        self.selected_range = content.len()..content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        self.composer_mentions = mentions;
        self.close_composer_menus();
        self.clear_validation_notice();
        self.sync_composer_input(window, cx);
        self.composer_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn cancel_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing) = self.editing_turn.take() else {
            return false;
        };
        self.edit_blur_subscription = None;
        self.restore_composer_snapshot(editing.saved_composer, cx);
        self.close_composer_menus();
        self.clear_validation_notice();
        self.sync_composer_input(window, cx);
        self.composer_input.focus_handle(cx).focus(window, cx);
        cx.notify();
        true
    }

    pub(super) fn cursor_offset(&self, cx: &App) -> usize {
        self.composer_input.read(cx).cursor()
    }
}
