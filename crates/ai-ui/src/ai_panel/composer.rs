use super::helpers::*;
use super::*;

impl AiPanel {
    fn input_is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window) || self.edit_focus_handle.is_focused(window)
    }

    pub(super) fn newline(
        &mut self,
        _: &input::Newline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model_menu_open {
            self.model_search_history.break_group();
            return;
        }
        if self.finish_page_number_edit(cx) {
            return;
        }
        if self.mode == AiPanelMode::Sidebar && self.confirm_mention_completion(cx) {
            return;
        }
        if self.input_is_focused(window) {
            self.replace_composer_selection("\n", EditGroup::Isolated, cx);
        }
    }

    pub(super) fn backspace(
        &mut self,
        _: &input::Backspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            let end = self.model_search_query.len();
            let start = previous_grapheme_boundary(&self.model_search_query, end);
            self.edit_model_search_query(start..end, "", EditGroup::Backspace, cx);
            return;
        }
        let before = self.composer_snapshot();
        if let Some(active) = self.active_page_number_edit.clone() {
            if self.selected_range.is_empty()
                && let Some(range) = mention_range_for_empty_page_number_backspace(
                    &active,
                    self.cursor_offset(),
                    &self.composer_mentions,
                )
            {
                self.apply_regular_edit(range, "");
                self.selection_reversed = false;
                self.marked_range = None;
                self.finish_composer_edit(before, EditGroup::Backspace, cx);
                return;
            }
            let range = if self.selected_range.is_empty() {
                let cursor = self.cursor_offset();
                if cursor <= active.digits.start {
                    self.composer_history.break_group();
                    return;
                }
                previous_grapheme_boundary(&self.content, cursor)..cursor
            } else {
                self.selected_range.clone()
            };
            if self.apply_page_number_edit(range, "") {
                self.selection_reversed = false;
                self.marked_range = None;
                self.finish_composer_edit(before, EditGroup::Backspace, cx);
            }
            return;
        }
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            if let Some(mention) = self
                .composer_mentions
                .iter()
                .find(|mention| mention.range.end == cursor)
            {
                self.selected_range = mention.range.clone();
            } else {
                self.selected_range = previous_grapheme_boundary(&self.content, cursor)..cursor;
                self.selection_reversed = true;
            }
        }
        let range = expand_range_to_mentions(self.selected_range.clone(), &self.composer_mentions);
        self.apply_regular_edit(range, "");
        self.selection_reversed = false;
        self.marked_range = None;
        self.finish_composer_edit(before, EditGroup::Backspace, cx);
    }

    pub(super) fn delete(
        &mut self,
        _: &input::Delete,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            self.model_search_history.break_group();
            return;
        }
        let before = self.composer_snapshot();
        if let Some(active) = self.active_page_number_edit.clone() {
            let range = if self.selected_range.is_empty() {
                let cursor = self.cursor_offset();
                if cursor >= active.digits.end {
                    self.composer_history.break_group();
                    return;
                }
                cursor..next_grapheme_boundary(&self.content, cursor).min(active.digits.end)
            } else {
                self.selected_range.clone()
            };
            if self.apply_page_number_edit(range, "") {
                self.selection_reversed = false;
                self.marked_range = None;
                self.finish_composer_edit(before, EditGroup::Delete, cx);
            }
            return;
        }
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            if let Some(mention) = self
                .composer_mentions
                .iter()
                .find(|mention| mention.range.start == cursor)
            {
                self.selected_range = mention.range.clone();
            } else {
                self.selected_range = cursor..next_grapheme_boundary(&self.content, cursor);
            }
        }
        let range = expand_range_to_mentions(self.selected_range.clone(), &self.composer_mentions);
        self.apply_regular_edit(range, "");
        self.selection_reversed = false;
        self.marked_range = None;
        self.finish_composer_edit(before, EditGroup::Delete, cx);
    }

    pub(super) fn delete_word_backward(
        &mut self,
        _: &input::DeleteWordBackward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            let end = self.model_search_query.len();
            let start = backward_word_deletion_start(&self.model_search_query, end);
            self.edit_model_search_query(start..end, "", EditGroup::Isolated, cx);
            return;
        }
        let before = self.composer_snapshot();
        let range = if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            let start = backward_word_deletion_start(&self.content, cursor);
            start..cursor
        } else {
            self.selected_range.clone()
        };
        let range = self.constrain_deletion_range(range);
        if range.is_empty() {
            self.composer_history.break_group();
            return;
        }
        if !self.apply_page_number_edit(range.clone(), "") {
            self.apply_regular_edit(expand_range_to_mentions(range, &self.composer_mentions), "");
        }
        self.selection_reversed = false;
        self.marked_range = None;
        self.finish_composer_edit(before, EditGroup::Isolated, cx);
    }

    pub(super) fn delete_word_forward(
        &mut self,
        _: &input::DeleteWordForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            self.model_search_history.break_group();
            return;
        }
        let before = self.composer_snapshot();
        let range = if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            cursor..forward_word_deletion_end(&self.content, cursor)
        } else {
            self.selected_range.clone()
        };
        let range = self.constrain_deletion_range(range);
        if range.is_empty() {
            self.composer_history.break_group();
            return;
        }
        if !self.apply_page_number_edit(range.clone(), "") {
            self.apply_regular_edit(expand_range_to_mentions(range, &self.composer_mentions), "");
        }
        self.selection_reversed = false;
        self.marked_range = None;
        self.finish_composer_edit(before, EditGroup::Isolated, cx);
    }

    pub(super) fn delete_to_line_start(
        &mut self,
        _: &input::DeleteToLineStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            let end = self.model_search_query.len();
            self.edit_model_search_query(0..end, "", EditGroup::Isolated, cx);
            return;
        }
        let cursor = self.cursor_offset();
        self.delete_semantic_range(line_start(&self.content, cursor)..cursor, cx);
    }

    pub(super) fn delete_to_line_end(
        &mut self,
        _: &input::DeleteToLineEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            self.model_search_history.break_group();
            return;
        }
        let cursor = self.cursor_offset();
        self.delete_semantic_range(cursor..line_end(&self.content, cursor), cx);
    }

    pub(super) fn left(&mut self, _: &input::Left, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            self.model_search_history.break_group();
            return;
        }
        if let Some(active) = self.active_page_number_edit.clone()
            && self.selected_range.is_empty()
            && self.cursor_offset() <= active.digits.start
        {
            self.finish_page_number_edit(cx);
            self.move_to(active.mention_start, cx);
            return;
        }
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            let target = self
                .composer_mentions
                .iter()
                .find(|mention| mention.range.end == cursor)
                .map_or_else(
                    || previous_grapheme_boundary(&self.content, cursor),
                    |mention| mention.range.start,
                );
            self.move_to(target, cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    pub(super) fn right(&mut self, _: &input::Right, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            self.model_search_history.break_group();
            return;
        }
        if let Some(active) = self.active_page_number_edit.clone()
            && self.selected_range.is_empty()
            && self.cursor_offset() >= active.digits.end
        {
            self.finish_page_number_edit(cx);
            return;
        }
        if self.selected_range.is_empty() {
            let cursor = self.selected_range.end;
            let target = self
                .composer_mentions
                .iter()
                .find(|mention| mention.range.start == cursor)
                .map_or_else(
                    || next_grapheme_boundary(&self.content, cursor),
                    |mention| mention.range.end,
                );
            self.move_to(target, cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    pub(super) fn up(&mut self, _: &input::Up, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            self.model_search_history.break_group();
            return;
        }
        self.composer_history.break_group();
        if self.select_previous_mention_completion(cx) {
            return;
        }
        self.move_vertically(-1, false, window, cx);
    }

    pub(super) fn down(&mut self, _: &input::Down, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            self.model_search_history.break_group();
            return;
        }
        self.composer_history.break_group();
        if self.select_next_mention_completion(cx) {
            return;
        }
        self.move_vertically(1, false, window, cx);
    }

    pub(super) fn home(&mut self, _: &input::Home, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        self.move_to(line_start(&self.content, self.cursor_offset()), cx);
    }

    pub(super) fn end(&mut self, _: &input::End, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        self.move_to(line_end(&self.content, self.cursor_offset()), cx);
    }

    pub(super) fn word_left(
        &mut self,
        _: &input::WordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        let target =
            mention_aware_word_left(&self.content, self.cursor_offset(), &self.composer_mentions);
        self.move_to(self.constrain_movement_target(target), cx);
    }

    pub(super) fn word_right(
        &mut self,
        _: &input::WordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        let target =
            mention_aware_word_right(&self.content, self.cursor_offset(), &self.composer_mentions);
        self.move_to(self.constrain_movement_target(target), cx);
    }

    pub(super) fn document_start(
        &mut self,
        _: &input::DocumentStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.input_is_focused(window) && !self.model_menu_open {
            self.move_to(self.constrain_movement_target(0), cx);
        }
    }

    pub(super) fn document_end(
        &mut self,
        _: &input::DocumentEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.input_is_focused(window) && !self.model_menu_open {
            self.move_to(self.constrain_movement_target(self.content.len()), cx);
        }
    }

    pub(super) fn select_all(
        &mut self,
        _: &input::SelectAll,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    pub(super) fn select_left(
        &mut self,
        _: &input::SelectLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        let cursor = self.cursor_offset();
        let target = self
            .composer_mentions
            .iter()
            .find(|mention| mention.range.end == cursor)
            .map_or_else(
                || previous_grapheme_boundary(&self.content, cursor),
                |mention| mention.range.start,
            );
        self.select_to(self.constrain_movement_target(target), cx);
    }

    pub(super) fn select_right(
        &mut self,
        _: &input::SelectRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        let cursor = self.cursor_offset();
        let target = self
            .composer_mentions
            .iter()
            .find(|mention| mention.range.start == cursor)
            .map_or_else(
                || next_grapheme_boundary(&self.content, cursor),
                |mention| mention.range.end,
            );
        self.select_to(self.constrain_movement_target(target), cx);
    }

    pub(super) fn select_word_left(
        &mut self,
        _: &input::SelectWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        let target =
            mention_aware_word_left(&self.content, self.cursor_offset(), &self.composer_mentions);
        self.select_to(self.constrain_movement_target(target), cx);
    }

    pub(super) fn select_word_right(
        &mut self,
        _: &input::SelectWordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        let target =
            mention_aware_word_right(&self.content, self.cursor_offset(), &self.composer_mentions);
        self.select_to(self.constrain_movement_target(target), cx);
    }

    pub(super) fn select_up(
        &mut self,
        _: &input::SelectUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        self.move_vertically(-1, true, window, cx);
    }

    pub(super) fn select_down(
        &mut self,
        _: &input::SelectDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        self.move_vertically(1, true, window, cx);
    }

    pub(super) fn select_home(
        &mut self,
        _: &input::SelectHome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        self.select_to(
            self.constrain_movement_target(line_start(&self.content, self.cursor_offset())),
            cx,
        );
    }

    pub(super) fn select_end(
        &mut self,
        _: &input::SelectEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        self.select_to(
            self.constrain_movement_target(line_end(&self.content, self.cursor_offset())),
            cx,
        );
    }

    pub(super) fn select_document_start(
        &mut self,
        _: &input::SelectDocumentStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.input_is_focused(window) && !self.model_menu_open {
            self.select_to(self.constrain_movement_target(0), cx);
        }
    }

    pub(super) fn select_document_end(
        &mut self,
        _: &input::SelectDocumentEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.input_is_focused(window) && !self.model_menu_open {
            self.select_to(self.constrain_movement_target(self.content.len()), cx);
        }
    }

    pub(super) fn copy(&mut self, _: &input::Copy, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        self.copy_selected_text(cx);
    }

    pub(super) fn cut(&mut self, _: &input::Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input_is_focused(window) || self.model_menu_open {
            return;
        }
        if self.copy_selected_text(cx) {
            self.delete_semantic_range(self.selected_range.clone(), cx);
        }
    }

    pub(super) fn paste(&mut self, _: &input::Paste, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input_is_focused(window) {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            if self.model_menu_open {
                let end = self.model_search_query.len();
                self.edit_model_search_query(end..end, &text, EditGroup::Isolated, cx);
                return;
            }
            self.replace_composer_selection(&text, EditGroup::Isolated, cx);
        }
    }

    pub(super) fn undo(&mut self, _: &input::Undo, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            if let Some(query) = self
                .model_search_history
                .undo(self.model_search_query.clone())
            {
                self.model_search_query = query;
                cx.notify();
            }
            return;
        }
        if let Some(snapshot) = self.composer_history.undo(self.composer_snapshot()) {
            self.restore_composer_snapshot(snapshot, cx);
        }
    }

    pub(super) fn redo(&mut self, _: &input::Redo, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input_is_focused(window) {
            return;
        }
        if self.model_menu_open {
            if let Some(query) = self
                .model_search_history
                .redo(self.model_search_query.clone())
            {
                self.model_search_query = query;
                cx.notify();
            }
            return;
        }
        if let Some(snapshot) = self.composer_history.redo(self.composer_snapshot()) {
            self.restore_composer_snapshot(snapshot, cx);
        }
    }

    pub(super) fn composer_snapshot(&self) -> ComposerSnapshot {
        ComposerSnapshot {
            content: self.content.clone(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
            marked_range: self.marked_range.clone(),
            mentions: self.composer_mentions.clone(),
            active_page_number_edit: self.active_page_number_edit.clone(),
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
        self.active_page_number_edit = snapshot.active_page_number_edit;
        self.reset_composer_layout();
        self.update_mention_completion(cx);
        cx.notify();
    }

    pub(super) fn finish_composer_edit(
        &mut self,
        before: ComposerSnapshot,
        group: EditGroup,
        cx: &mut Context<Self>,
    ) {
        let after = self.composer_snapshot();
        self.composer_history.record(before, &after, group);
        self.update_mention_completion(cx);
        cx.notify();
    }

    fn replace_composer_selection(
        &mut self,
        new_text: &str,
        group: EditGroup,
        cx: &mut Context<Self>,
    ) {
        let before = self.composer_snapshot();
        let range = self.selected_range.clone();
        if !self.apply_page_number_edit(range.clone(), new_text) {
            self.apply_regular_edit(range, new_text);
        }
        self.selection_reversed = false;
        self.marked_range = None;
        self.finish_composer_edit(before, group, cx);
    }

    fn delete_semantic_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let before = self.composer_snapshot();
        let range = if self.selected_range.is_empty() {
            range
        } else {
            self.selected_range.clone()
        };
        let range = self.constrain_deletion_range(range);
        if range.is_empty() {
            self.composer_history.break_group();
            return;
        }
        if !self.apply_page_number_edit(range.clone(), "") {
            self.apply_regular_edit(expand_range_to_mentions(range, &self.composer_mentions), "");
        }
        self.selection_reversed = false;
        self.marked_range = None;
        self.finish_composer_edit(before, EditGroup::Isolated, cx);
    }

    pub(super) fn constrain_deletion_range(&self, range: Range<usize>) -> Range<usize> {
        let Some(active) = self.active_page_number_edit.as_ref() else {
            return range;
        };
        range.start.clamp(active.digits.start, active.digits.end)
            ..range.end.clamp(active.digits.start, active.digits.end)
    }

    fn constrain_movement_target(&self, target: usize) -> usize {
        self.active_page_number_edit
            .as_ref()
            .map_or(target, |active| {
                target.clamp(active.digits.start, active.digits.end)
            })
    }
    fn reset_composer_layout(&mut self) {
        self.last_display = None;
        self.last_lines.clear();
        self.last_bounds = None;
        self.last_cursor_pos = None;
        self.mention_completion = None;
        self.invalidate_mention_preview();
    }

    pub(super) fn start_editing_message(
        &mut self,
        message_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = self
            .messages
            .get(message_index)
            .map_or(0, |message| message.display_text().len());
        self.start_editing_message_at(message_index, cursor, 1, window, cx);
    }

    pub(super) fn start_editing_message_at(
        &mut self,
        message_index: usize,
        source_offset: usize,
        click_count: usize,
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
            saved_composer: self.composer_snapshot(),
        });
        self.content = content.clone().into();
        self.selected_range =
            selection_range_for_click(&content, &mentions, source_offset, click_count);
        self.selection_reversed = false;
        self.marked_range = None;
        self.composer_mentions = mentions;
        self.active_page_number_edit = None;
        self.composer_history.clear();
        self.close_composer_menus();
        self.clear_validation_notice();
        self.reset_composer_layout();
        self.edit_blur_subscription =
            Some(cx.on_blur(&self.edit_focus_handle, window, |_, _, cx| cx.notify()));
        self.edit_focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(super) fn cancel_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing) = self.editing_turn.take() else {
            return false;
        };
        self.edit_blur_subscription = None;
        self.restore_composer_snapshot(editing.saved_composer, cx);
        self.composer_history.clear();
        self.close_composer_menus();
        self.clear_validation_notice();
        self.focus_handle.focus(window, cx);
        cx.notify();
        true
    }
    pub(super) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing_turn.is_some() {
            self.edit_focus_handle.focus(window, cx);
        } else {
            self.focus_handle.focus(window, cx);
        }
        self.close_composer_menus();
        self.hide_mention_preview(cx);
        self.drag_origin = Some(event.position);

        let mut offset = self.index_for_mouse_position(event.position, window);
        if self
            .active_page_number_edit
            .as_ref()
            .is_some_and(|active| offset < active.digits.start || offset > active.digits.end)
            && self.finish_page_number_edit(cx)
        {
            offset = self.index_for_mouse_position(event.position, window);
        }
        if event.click_count >= 2 && !event.modifiers.shift {
            self.composer_history.break_group();
            let range = selection_range_for_click(
                &self.content,
                &self.composer_mentions,
                offset,
                event.click_count,
            );
            self.selected_range = self.constrain_deletion_range(range);
            self.selection_reversed = false;
            self.marked_range = None;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if !event.modifiers.shift
            && let Some(mention) = self.mention_at_offset(offset).cloned()
        {
            if let Some(digits) = Self::page_digits_range(&mention)
                && digits.start <= offset
                && offset <= digits.end
            {
                self.begin_page_number_edit(mention.range.start, cx);
                self.selected_range = offset..offset;
                cx.notify();
                cx.stop_propagation();
                return;
            }
            let midpoint = mention.range.start + mention.range.len() / 2;
            self.move_to(
                if offset < midpoint {
                    mention.range.start
                } else {
                    mention.range.end
                },
                cx,
            );
            cx.stop_propagation();
            return;
        }
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
        cx.stop_propagation();
    }

    pub(super) fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.drag_origin = None;
        cx.stop_propagation();
    }

    pub(super) fn on_mouse_up_out(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.drag_origin = None;
    }

    pub(super) fn on_mouse_down_out(
        &mut self,
        _: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_selection(cx);
    }

    pub(super) fn on_message_edit_mouse_down_out(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .mention_preview_bounds
            .get()
            .is_some_and(|bounds| bounds.contains(&event.position))
        {
            return;
        }
        let completion_matches = self.mention_completion_matches(cx);
        if self
            .completion_menu_bounds(&completion_matches, window)
            .is_some_and(|bounds| bounds.contains(&event.position))
        {
            return;
        }
        self.cancel_editing(window, cx);
    }

    pub(super) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            self.drag_origin = None;
            let source = self
                .editing_turn
                .as_ref()
                .map_or(MentionPreviewSource::Composer, |editing| {
                    MentionPreviewSource::Message(editing.message_index)
                });
            let hovered = self
                .mention_hitboxes
                .iter()
                .find(|hitbox| hitbox.bounds.contains(&event.position))
                .cloned()
                .and_then(|hitbox| {
                    self.composer_mentions
                        .iter()
                        .find(|mention| mention.range == hitbox.source_range)
                        .cloned()
                        .map(|mention| (hitbox, mention))
                });
            self.update_mention_preview(source, hovered, window, cx);
            return;
        }

        self.hide_mention_preview(cx);
        let should_select = if let Some(origin) = self.drag_origin.as_ref() {
            (event.position.x - origin.x).as_f32().abs() > DRAG_SELECT_THRESHOLD
                || (event.position.y - origin.y).as_f32().abs() > DRAG_SELECT_THRESHOLD
        } else {
            false
        };

        if should_select {
            self.select_to(self.index_for_mouse_position(event.position, window), cx);
            cx.stop_propagation();
        }
    }

    pub(super) fn on_panel_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.dragging() {
            return;
        }
        let hovered = self
            .message_mention_hitboxes
            .iter()
            .find(|target| target.hitbox.bounds.contains(&event.position))
            .cloned()
            .map(|target| {
                let source = MentionPreviewSource::Message(target.message_index);
                let hovered = (target.hitbox, target.mention);
                let hovered = if let Some(message) = self.messages.get(target.message_index) {
                    historical_preview_mention(message, hovered)
                } else {
                    hovered
                };
                (source, hovered)
            });
        if let Some((source, hovered)) = hovered {
            self.update_mention_preview(source, Some(hovered), window, cx);
        } else if self
            .mention_preview
            .as_ref()
            .is_some_and(|preview| matches!(preview.source, MentionPreviewSource::Message(_)))
        {
            self.schedule_mention_preview_hide(window, cx);
        }
    }

    fn cancel_selection(&mut self, cx: &mut Context<Self>) {
        self.drag_origin = None;
        if !self.selected_range.is_empty() {
            self.move_to(self.cursor_offset(), cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.composer_history.break_group();
        let offset = self
            .constrain_movement_target(offset)
            .min(self.content.len());
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.update_mention_completion(cx);
        cx.notify();
    }

    pub(super) fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.composer_history.break_group();
        let offset = self
            .constrain_movement_target(offset)
            .min(self.content.len());
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        };

        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }

        self.update_mention_completion(cx);
        cx.notify();
    }

    fn move_vertically(
        &mut self,
        direction: isize,
        extend_selection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(bounds), Some(cursor)) = (
            self.last_bounds,
            self.position_for_offset(self.cursor_offset(), window),
        ) else {
            return;
        };

        let line_height = window.line_height();
        let target = point(
            bounds.left() + cursor.x,
            bounds.top() + cursor.y + line_height * direction as f32,
        );
        let offset = self.index_for_mouse_position(target, window);
        if extend_selection {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    fn copy_selected_text(&self, cx: &mut Context<Self>) -> bool {
        if self.selected_range.is_empty() {
            return false;
        }

        cx.write_to_clipboard(ClipboardItem::new_string(
            self.content[self.selected_range.clone()].to_string(),
        ));
        true
    }
}
