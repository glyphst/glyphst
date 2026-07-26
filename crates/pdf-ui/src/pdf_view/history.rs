use super::*;

impl PdfView {
    pub(super) fn record_current_history_position(&self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let position = self.current_document_position();
        cx.global_mut::<LibraryStore>()
            .record_history(&path, position);
    }

    pub fn flush_current_history_position(&mut self, cx: &mut Context<Self>) {
        self.stage_current_history_position(cx);
        Self::persist_history_entry(&self.path, cx);
    }

    pub fn stage_current_history_position(&mut self, cx: &mut Context<Self>) {
        self.position_save_task.take();
        if self.loading_error.is_some() {
            return;
        }

        let position = if self.loading {
            self.pending_semantic_position
                .clone()
                .or_else(|| self.pending_initial_position.clone())
                .unwrap_or_else(|| self.current_document_position())
        } else {
            self.current_document_position()
        };
        cx.global_mut::<LibraryStore>()
            .record_history(&self.path, position);
    }

    pub(super) fn persist_history_entry(path: &Path, cx: &mut Context<Self>) {
        if let Some(snapshot) = cx
            .global::<LibraryStore>()
            .history_entry_persistence_snapshot(path)
        {
            cx.background_spawn(async move {
                if let Err(error) = snapshot.persist() {
                    eprintln!("failed to save document history: {error:#}");
                }
            })
            .detach();
        }
    }

    pub(super) fn schedule_position_save(&mut self, cx: &mut Context<Self>) {
        self.pending_semantic_position = None;
        self.pending_initial_position = None;
        self.record_current_history_position(cx);
        self.position_save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1000))
                .await;
            _ = this.update(cx, |this, cx| {
                Self::persist_history_entry(&this.path, cx);
            });
        }));
    }

    fn current_sidebar_widths(&self) -> SidebarWidths {
        SidebarWidths {
            thumbnails: self.thumbnail_sidebar_width,
            toc: self.toc_sidebar_width,
            notes: self.notes_sidebar_width,
            ai: self.ai_panel_width,
        }
    }

    pub(super) fn schedule_sidebar_width_save(&mut self, cx: &mut Context<Self>) {
        self.sidebar_width_save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            _ = this.update(cx, |this, cx| {
                let widths = Self::sanitize_sidebar_widths(this.current_sidebar_widths());
                if let Err(error) = cx.global_mut::<LibraryStore>().set_sidebar_widths(widths) {
                    eprintln!("failed to save sidebar widths: {error:#}");
                }
            });
        }));
    }

    pub(super) fn current_jump_position(&self) -> JumpPosition {
        if let Some(page_idx) = self.presentation_page_index() {
            return JumpPosition {
                scroll_x_doc: 0.0,
                scroll_y_doc: self.page_scroll_y_doc(page_idx),
            };
        }

        JumpPosition {
            scroll_x_doc: self.scroll_x_doc,
            scroll_y_doc: self.scroll_y_doc,
        }
    }

    pub(super) fn push_jump_position(stack: &mut Vec<JumpPosition>, position: JumpPosition) {
        if stack.last().is_some_and(|last| {
            (last.scroll_x_doc - position.scroll_x_doc).abs() <= JUMP_POSITION_EPSILON_DOC
                && (last.scroll_y_doc - position.scroll_y_doc).abs() <= JUMP_POSITION_EPSILON_DOC
        }) {
            return;
        }

        stack.push(position);
        if stack.len() > MAX_JUMP_HISTORY {
            stack.remove(0);
        }
    }

    pub(super) fn record_search_jump_origin(&mut self) {
        if self.position_from_search {
            return;
        }

        let position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, position);
        self.jump_forward.clear();
        self.position_from_search = true;
    }

    pub(super) fn mark_manual_navigation(&mut self) {
        self.position_from_search = false;
        self.search.cancel_reveal_after_indexing();
    }

    pub(super) fn jump_backward(&mut self, cx: &mut Context<Self>) {
        let Some(position) = self.jump_back.pop() else {
            return;
        };

        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_forward, current_position);
        if self.is_presentation_mode() {
            self.set_presentation_page(self.page_index_for_doc_y(position.scroll_y_doc), cx);
        } else {
            self.set_scroll_doc(position.scroll_x_doc, position.scroll_y_doc, cx);
        }
        self.mark_manual_navigation();
    }

    pub(super) fn jump_forward(&mut self, cx: &mut Context<Self>) {
        let Some(position) = self.jump_forward.pop() else {
            return;
        };

        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        if self.is_presentation_mode() {
            self.set_presentation_page(self.page_index_for_doc_y(position.scroll_y_doc), cx);
        } else {
            self.set_scroll_doc(position.scroll_x_doc, position.scroll_y_doc, cx);
        }
        self.mark_manual_navigation();
    }
}
