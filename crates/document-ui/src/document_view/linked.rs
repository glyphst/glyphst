use super::*;

impl DocumentView {
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

    pub fn apply_live_translation_hover(
        &mut self,
        session_id: DocumentTranslationJobId,
        region_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.translation_view_role != TranslationViewRole::SourceMirror
            || self.translation_pair_session_id != Some(session_id)
            || !self.translation_pair_active
        {
            return;
        }
        if self.external_hovered_translation_region != region_id {
            self.external_hovered_translation_region = region_id;
            cx.notify();
        }
    }

    fn publish_hovered_translation_region(
        &mut self,
        region_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.translation_view_role != TranslationViewRole::Translation
            || !self.translation_pair_active
            || self.hovered_translation_region == region_id
        {
            return;
        }
        self.hovered_translation_region = region_id.clone();
        if !self.suppress_linked_events
            && let Some(session_id) = self.translation_pair_session_id
        {
            cx.emit(DocumentViewEvent::TranslationRegionHovered {
                session_id,
                region_id,
            });
        }
        cx.notify();
    }

    fn cancel_translation_hover_clear(&mut self) {
        self.translation_hover_clear_generation =
            self.translation_hover_clear_generation.wrapping_add(1);
        self.translation_hover_clear_task = None;
    }

    pub(super) fn set_hovered_translation_region(
        &mut self,
        region_id: String,
        cx: &mut Context<Self>,
    ) {
        self.cancel_translation_hover_clear();
        self.publish_hovered_translation_region(Some(region_id), cx);
    }

    pub(super) fn schedule_translation_hover_clear(&mut self, cx: &mut Context<Self>) {
        if self.translation_view_role != TranslationViewRole::Translation
            || !self.translation_pair_active
            || self.translation_hover_tools_hovered
            || self.hovered_translation_region.is_none()
            || self.translation_hover_clear_task.is_some()
        {
            return;
        }
        self.translation_hover_clear_generation =
            self.translation_hover_clear_generation.wrapping_add(1);
        let generation = self.translation_hover_clear_generation;
        self.translation_hover_clear_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(TRANSLATION_HOVER_CLEAR_GRACE)
                .await;
            _ = this.update(cx, |this, cx| {
                if this.translation_hover_clear_generation != generation
                    || this.translation_hover_tools_hovered
                {
                    return;
                }
                this.translation_hover_clear_task = None;
                this.publish_hovered_translation_region(None, cx);
            });
        }));
    }

    pub(super) fn set_translation_hover_tools_hovered(
        &mut self,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if self.translation_hover_tools_hovered == hovered {
            return;
        }
        self.translation_hover_tools_hovered = hovered;
        if hovered {
            self.cancel_translation_hover_clear();
        } else {
            self.schedule_translation_hover_clear(cx);
        }
    }

    pub(super) fn clear_translation_hover_state(
        &mut self,
        reset_animation: bool,
        cx: &mut Context<Self>,
    ) {
        self.cancel_translation_hover_clear();
        self.translation_hover_tools_hovered = false;
        self.publish_hovered_translation_region(None, cx);
        self.external_hovered_translation_region = None;
        if reset_animation {
            self.translation_hover_animation = None;
        }
    }
}
