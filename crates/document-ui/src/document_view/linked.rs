use super::*;

impl DocumentView {
    pub fn set_translated_view(&mut self, translated: bool, cx: &mut Context<Self>) {
        if translated && !self.capabilities.translated_document {
            return;
        }
        self.view_language = if translated {
            DocumentViewLanguage::Translated
        } else {
            DocumentViewLanguage::Original
        };
        if translated {
            self.request_document_translation(cx);
        }
        cx.notify();
    }

    fn viewport_center_pdf_anchor(&self) -> Option<(usize, mupdf::Point)> {
        if self.pages.is_empty() {
            return None;
        }
        let scale = self.effective_scale();
        let center_y = self.target_scroll_y_doc + self.viewport_height / (2.0 * scale);
        let page_idx = self.page_index_for_doc_y(center_y);
        let page = self.pages.get(page_idx)?;
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let point = mupdf::Point {
            x: (page_width / 2.0 + self.target_scroll_x_doc).clamp(0.0, page_width),
            y: (center_y - page.offset_y).clamp(0.0, page_height),
        };
        Some((
            page_idx,
            Self::view_point_to_pdf(page.bounds, self.rotation, point),
        ))
    }

    fn set_rotation_state(&mut self, rotation: DocumentRotation, cx: &mut Context<Self>) {
        if self.rotation == rotation || self.capabilities.reflow {
            return;
        }
        self.rotation = rotation;
        self.pending_renders.clear();
        self.page_image_bounds.fill(None);
        self.diff_flashes.clear();
        self.diff_pending_pages.clear();
        self.translated_text_layouts.clear();
        self.region_drag = None;
        self.context_menu_position = None;
        self.context_menu_popup = None;
        self.jump_back.clear();
        self.jump_forward.clear();
        let page_gap = cx.global::<SettingsStore>().resolved_theme.page_gap;
        self.recompute_layout(page_gap);
        self.base_scale = self.viewport_width.max(1.0) / self.max_page_width.max(1.0);
        cx.notify();
    }

    pub(super) fn apply_stored_rotation(
        &mut self,
        rotation: DocumentRotation,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.rotation == rotation || self.capabilities.reflow {
            return false;
        }
        let anchor = self.viewport_center_pdf_anchor();
        self.set_rotation_state(rotation, cx);
        if !self.is_presentation_mode()
            && let Some((page_idx, source_point)) = anchor
            && let Some(page) = self.pages.get(page_idx)
        {
            let point = Self::pdf_point_to_view(page.bounds, self.rotation, source_point);
            let (page_width, _) = self.page_dimensions(page_idx);
            let scale = self.effective_scale();
            self.scroll_x_doc = point.x - page_width / 2.0;
            self.scroll_y_doc = page.offset_y + point.y - self.viewport_height / (2.0 * scale);
            self.target_scroll_x_doc = self.scroll_x_doc;
            self.target_scroll_y_doc = self.scroll_y_doc;
            self.clamp_scroll();
        }
        self.schedule_position_save(cx);
        true
    }

    fn rotate_to(&mut self, rotation: DocumentRotation, cx: &mut Context<Self>) {
        if self.rotation == rotation || self.capabilities.reflow {
            return;
        }
        match cx.update_global::<LibraryStore, _>(|store, _| {
            store.set_document_rotation(&self.path, rotation)
        }) {
            Ok(true) => {
                self.apply_stored_rotation(rotation, cx);
            }
            Ok(false) => {}
            Err(error) => {
                eprintln!("failed to save document rotation: {error:#}");
            }
        }
    }

    pub(super) fn rotate_clockwise(&mut self, cx: &mut Context<Self>) {
        self.rotate_to(self.rotation.clockwise(), cx);
    }

    pub(super) fn rotate_counterclockwise(&mut self, cx: &mut Context<Self>) {
        self.rotate_to(self.rotation.counterclockwise(), cx);
    }

    pub fn apply_linked_viewport(&mut self, viewport: LinkedViewport, cx: &mut Context<Self>) {
        if self.pages.get(viewport.page_idx).is_none() {
            return;
        }
        self.suppress_linked_events = true;
        self.set_rotation_state(viewport.rotation, cx);
        self.zoom = viewport.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let page = &self.pages[viewport.page_idx];
        let (_, page_height) = self.page_dimensions(viewport.page_idx);
        self.set_scroll_doc(
            self.scroll_x_doc,
            page.offset_y + viewport.y_in_page.max(0.0).min(page_height),
            cx,
        );
        self.suppress_linked_events = false;
    }

    fn linked_block_anchor(&self, block_id: u32) -> Option<LinkedBlockAnchor> {
        let block = self.text_blocks.iter().find(|block| block.id == block_id)?;
        Some(LinkedBlockAnchor {
            block_id,
            page_idx: block.page_idx,
            x0: block.bounds.x0,
            y0: block.bounds.y0,
            x1: block.bounds.x1,
            y1: block.bounds.y1,
        })
    }

    fn map_linked_block(&self, anchor: LinkedBlockAnchor) -> Option<u32> {
        let anchor_center = ((anchor.x0 + anchor.x1) * 0.5, (anchor.y0 + anchor.y1) * 0.5);
        self.text_blocks
            .iter()
            .filter(|block| block.page_idx == anchor.page_idx)
            .min_by(|left, right| {
                let distance = |block: &TextBlock| {
                    let center = (
                        (block.bounds.x0 + block.bounds.x1) * 0.5,
                        (block.bounds.y0 + block.bounds.y1) * 0.5,
                    );
                    (center.0 - anchor_center.0).powi(2) + (center.1 - anchor_center.1).powi(2)
                };
                distance(left).total_cmp(&distance(right))
            })
            .map(|block| block.id)
    }

    pub fn apply_linked_hover(
        &mut self,
        anchor: Option<LinkedBlockAnchor>,
        cx: &mut Context<Self>,
    ) {
        let block_id = anchor.and_then(|anchor| self.map_linked_block(anchor));
        if self.external_hovered_block != block_id {
            self.external_hovered_block = block_id;
            cx.notify();
        }
    }

    pub fn apply_linked_selection(
        &mut self,
        selection: Option<LinkedBlockSelection>,
        cx: &mut Context<Self>,
    ) {
        let block_id = selection.and_then(|selection| self.map_linked_block(selection.anchor));
        if self.external_selected_block != block_id {
            self.external_selected_block = block_id;
            cx.notify();
        }
    }

    pub(super) fn emit_linked_viewport(&mut self, cx: &mut Context<Self>) {
        if self.suppress_linked_events || self.pages.is_empty() {
            return;
        }
        self.linked_viewport_generation = self.linked_viewport_generation.wrapping_add(1);
        let page_idx = self.page_index_for_scroll_y(self.target_scroll_y_doc);
        let page = &self.pages[page_idx];
        let (_, page_height) = self.page_dimensions(page_idx);
        cx.emit(DocumentViewEvent::ViewportChanged(LinkedViewport {
            page_idx,
            y_in_page: (self.target_scroll_y_doc - page.offset_y).clamp(0.0, page_height),
            zoom: self.zoom,
            rotation: self.rotation,
            generation: self.linked_viewport_generation,
        }));
    }

    pub(super) fn set_hovered_text_block(&mut self, block_id: Option<u32>, cx: &mut Context<Self>) {
        if self.hovered_text_block != block_id {
            self.hovered_text_block = block_id;
            if !self.suppress_linked_events {
                cx.emit(DocumentViewEvent::BlockHovered(
                    block_id.and_then(|block_id| self.linked_block_anchor(block_id)),
                ));
            }
            cx.notify();
        }
    }

    pub(super) fn emit_linked_selection(&mut self, cx: &mut Context<Self>) {
        if self.suppress_linked_events {
            return;
        }
        self.linked_selection_generation = self.linked_selection_generation.wrapping_add(1);
        let selection = self.selection_bounds().and_then(|(start, _)| {
            self.glyphs.get(start).and_then(|glyph| {
                self.linked_block_anchor(glyph.block_idx)
                    .map(|anchor| LinkedBlockSelection {
                        anchor,
                        generation: self.linked_selection_generation,
                    })
            })
        });
        cx.emit(DocumentViewEvent::BlockSelected(selection));
    }
}
