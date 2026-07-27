use super::*;

impl DocumentView {
    pub(super) fn max_scroll_y_doc(&self) -> f32 {
        let viewport_height_doc = self.viewport_height / self.effective_scale();
        (self.content_height_doc - viewport_height_doc).max(0.0)
    }

    pub(super) fn max_scroll_x_doc(&self) -> f32 {
        let viewport_width_doc = self.viewport_width / self.effective_scale();
        ((self.max_page_width - viewport_width_doc) / 2.0).max(0.0)
    }

    pub(super) fn recompute_layout(&mut self, page_gap: f32) {
        let mut offset_y = 0.0;
        let mut max_page_width = 0.0_f32;
        for page in &mut self.pages {
            let (width, height) = Self::page_dimensions_for(page, self.rotation);
            page.offset_y = offset_y;
            offset_y += height + page_gap;
            max_page_width = max_page_width.max(width);
        }

        self.max_page_width = max_page_width.max(1.0);
        self.content_height_doc = if self.pages.is_empty() {
            0.0
        } else {
            offset_y - page_gap
        };
        self.clamp_scroll();
    }

    pub(super) fn schedule_reflow_for_viewport(
        &mut self,
        viewport_width: f32,
        cx: &mut Context<Self>,
    ) {
        if !self.capabilities.reflow {
            return;
        }
        let desired_width = self
            .epub_preferences
            .layout_for_viewport(viewport_width, 960.0)
            .width
            .round();
        let current_width = match self.document_layout {
            DocumentLayout::Reflow(layout) => layout.width.round(),
            DocumentLayout::Fixed => return,
        };
        if (desired_width - current_width).abs() < 1.0 {
            return;
        }

        self.reflow_resize_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            _ = this.update(cx, |this, cx| {
                this.reflow_resize_task = None;
                this.reflow_to_viewport(desired_width, cx);
            });
        }));
    }

    fn reflow_to_viewport(&mut self, viewport_width: f32, cx: &mut Context<Self>) {
        if !self.capabilities.reflow {
            return;
        }
        let layout = self
            .epub_preferences
            .layout_for_viewport(viewport_width, 960.0);
        if self.document_layout == DocumentLayout::Reflow(layout) {
            return;
        }
        let position = self
            .pending_semantic_position
            .clone()
            .or_else(|| self.pending_initial_position.clone())
            .filter(DocumentPosition::is_reflow)
            .unwrap_or_else(|| self.current_document_position());
        self.pending_initial_position = Some(position);
        self.pending_renders.clear();
        self.document_layout = DocumentLayout::Reflow(layout);
        self.request_reload(cx);
    }
}
