use super::*;

impl DocumentView {
    pub(super) fn is_two_column_layout(&self) -> bool {
        self.capabilities.two_column_layout && self.page_layout == DocumentPageLayout::TwoColumn
    }

    pub(super) fn max_scroll_y_doc(&self) -> f32 {
        let viewport_height_doc = self.viewport_height / self.effective_scale();
        (self.content_height_doc - viewport_height_doc).max(0.0)
    }

    pub(super) fn max_scroll_x_doc(&self) -> f32 {
        let viewport_width_doc = self.viewport_width / self.effective_scale();
        ((self.max_page_width - viewport_width_doc) / 2.0).max(0.0)
    }

    pub(super) fn recompute_layout(&mut self, page_gap: f32) {
        self.page_gap = page_gap.max(0.0);
        let page_layout = if self.is_two_column_layout() {
            DocumentPageLayout::TwoColumn
        } else {
            DocumentPageLayout::SingleColumn
        };
        (self.max_page_width, self.content_height_doc) =
            Self::layout_pages(&mut self.pages, page_layout, self.rotation, self.page_gap);
        self.clamp_scroll();
    }

    pub(super) fn layout_pages(
        pages: &mut [DocumentPage],
        page_layout: DocumentPageLayout,
        rotation: DocumentRotation,
        page_gap: f32,
    ) -> (f32, f32) {
        match page_layout {
            DocumentPageLayout::SingleColumn => {
                Self::layout_single_column_pages(pages, rotation, page_gap)
            }
            DocumentPageLayout::TwoColumn => {
                Self::layout_two_column_pages(pages, rotation, page_gap)
            }
        }
    }

    fn layout_single_column_pages(
        pages: &mut [DocumentPage],
        rotation: DocumentRotation,
        page_gap: f32,
    ) -> (f32, f32) {
        let max_page_width = pages
            .iter()
            .map(|page| Self::page_dimensions_for(page, rotation).0)
            .fold(0.0_f32, f32::max);
        let mut offset_y = 0.0;
        for page in pages.iter_mut() {
            let (width, height) = Self::page_dimensions_for(page, rotation);
            page.offset_x = (max_page_width - width) / 2.0;
            page.offset_y = offset_y;
            offset_y += height + page_gap;
        }

        let content_height = if pages.is_empty() {
            0.0
        } else {
            offset_y - page_gap
        };
        (max_page_width.max(1.0), content_height)
    }

    fn layout_two_column_pages(
        pages: &mut [DocumentPage],
        rotation: DocumentRotation,
        page_gap: f32,
    ) -> (f32, f32) {
        let mut left_width = 0.0_f32;
        let mut right_width = 0.0_f32;
        for (page_idx, page) in pages.iter().enumerate() {
            let width = Self::page_dimensions_for(page, rotation).0;
            if page_idx % 2 == 0 {
                left_width = left_width.max(width);
            } else {
                right_width = right_width.max(width);
            }
        }
        if pages.len() % 2 == 1 {
            right_width = right_width.max(left_width);
        }

        let mut offset_y = 0.0;
        for row_start in (0..pages.len()).step_by(2) {
            let row_end = (row_start + 2).min(pages.len());
            let row_height = pages[row_start..row_end]
                .iter()
                .map(|page| Self::page_dimensions_for(page, rotation).1)
                .fold(0.0_f32, f32::max);

            for (row_offset, page) in pages[row_start..row_end].iter_mut().enumerate() {
                let page_idx = row_start + row_offset;
                let width = Self::page_dimensions_for(page, rotation).0;
                page.offset_x = if page_idx % 2 == 0 {
                    (left_width - width) / 2.0
                } else {
                    left_width + page_gap + (right_width - width) / 2.0
                };
                page.offset_y = offset_y;
            }
            offset_y += row_height + page_gap;
        }

        let content_height = if pages.is_empty() {
            0.0
        } else {
            offset_y - page_gap
        };
        (
            (left_width + page_gap + right_width).max(1.0),
            content_height,
        )
    }

    pub(super) fn spread_start_page(&self, page_idx: usize) -> usize {
        let page_idx = page_idx.min(self.last_page_index());
        if self.is_two_column_layout() {
            page_idx - page_idx % 2
        } else {
            page_idx
        }
    }

    pub(super) fn spread_page_range(&self, page_idx: usize) -> Range<usize> {
        let start = self.spread_start_page(page_idx);
        let end = if self.is_two_column_layout() {
            (start + 2).min(self.pages.len())
        } else {
            (start + 1).min(self.pages.len())
        };
        start..end
    }

    pub(super) fn page_index_for_doc_point(&self, x: f32, y: f32) -> usize {
        self.pages
            .iter()
            .enumerate()
            .find_map(|(page_idx, page)| {
                let (width, height) = Self::page_dimensions_for(page, self.rotation);
                (x >= page.offset_x
                    && x <= page.offset_x + width
                    && y >= page.offset_y
                    && y <= page.offset_y + height)
                    .then_some(page_idx)
            })
            .unwrap_or_else(|| self.page_index_for_doc_y(y))
    }

    pub(super) fn apply_stored_page_layout(
        &mut self,
        page_layout: DocumentPageLayout,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.capabilities.two_column_layout || self.page_layout == page_layout {
            return false;
        }

        let anchor = self.viewport_center_pdf_anchor();
        self.page_layout = page_layout;
        self.pending_renders.clear();
        self.page_image_bounds.fill(None);
        self.region_drag = None;
        self.selected_region = None;
        self.context_menu_region = false;
        self.recompute_layout(cx.global::<SettingsStore>().resolved_theme.page_gap);
        self.base_scale = self.viewport_width.max(1.0) / self.max_page_width.max(1.0);

        if !self.is_presentation_mode()
            && let Some((page_idx, source_point)) = anchor
            && let Some(page) = self.pages.get(page_idx)
        {
            let point = Self::pdf_point_to_view(page.bounds, self.rotation, source_point);
            let scale = self.effective_scale();
            self.scroll_x_doc = page.offset_x + point.x - self.max_page_width / 2.0;
            self.scroll_y_doc = page.offset_y + point.y - self.viewport_height / (2.0 * scale);
            self.target_scroll_x_doc = self.scroll_x_doc;
            self.target_scroll_y_doc = self.scroll_y_doc;
            self.clamp_scroll();
        }

        self.schedule_position_save(cx);
        self.emit_live_translation_viewport_changed(SynchronizedScrollMode::Immediate, cx);
        cx.notify();
        true
    }

    pub(super) fn toggle_two_column_mode(&mut self, cx: &mut Context<Self>) {
        if !self.capabilities.two_column_layout {
            return;
        }
        let page_layout = match self.page_layout {
            DocumentPageLayout::SingleColumn => DocumentPageLayout::TwoColumn,
            DocumentPageLayout::TwoColumn => DocumentPageLayout::SingleColumn,
        };
        match cx.update_global::<LibraryStore, _>(|store, _| {
            store.set_document_page_layout(&self.path, page_layout)
        }) {
            Ok(true) => {
                self.apply_stored_page_layout(page_layout, cx);
            }
            Ok(false) => {}
            Err(error) => {
                eprintln!("failed to save document page layout: {error:#}");
            }
        }
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
        let layout = self.epub_layout_for_viewport(self.epub_preferences, viewport_width, 960.0);
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
