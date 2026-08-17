use super::*;

impl DocumentView {
    pub(super) fn translation_viewport_geometry_ready(&self) -> bool {
        self.viewport_width.is_finite()
            && self.viewport_height.is_finite()
            && self.viewport_width > 1.0
            && self.viewport_height > 1.0
            && self.effective_scale().is_finite()
    }

    pub fn live_translation_viewport_anchor(
        &self,
        mode: SynchronizedScrollMode,
    ) -> Option<TranslationViewportAnchor> {
        if self.format != DocumentFormat::Pdf
            || self.capabilities.reflow
            || self.translation.view_role == TranslationViewRole::None
            || !self.translation.position_sync_ready
            || !self.translation_viewport_geometry_ready()
            || self.pages.is_empty()
        {
            return None;
        }

        let (page_idx, center_x, anchor_y, viewport_y_fraction) =
            if let Some(page_idx) = self.presentation_page_index() {
                let (page_width, page_height) = self.page_dimensions(page_idx);
                (page_idx, page_width / 2.0, page_height / 2.0, 0.5)
            } else {
                let (scroll_x_doc, scroll_y_doc) = match mode {
                    SynchronizedScrollMode::Immediate => (self.scroll_x_doc, self.scroll_y_doc),
                    SynchronizedScrollMode::Target => {
                        (self.target_scroll_x_doc, self.target_scroll_y_doc)
                    }
                };
                let max_scroll_y = self.max_scroll_y_doc();
                let viewport_y_fraction = if max_scroll_y <= 0.01 || scroll_y_doc <= 0.01 {
                    0.0
                } else if max_scroll_y - scroll_y_doc <= 0.01 {
                    1.0
                } else {
                    0.5
                };
                let viewport_height_doc = self.viewport_height / self.effective_scale();
                let anchor_doc_y = scroll_y_doc + viewport_height_doc * viewport_y_fraction;
                let anchor_doc_x = self.max_page_width / 2.0 + scroll_x_doc;
                let page_idx = self.page_index_for_doc_point(anchor_doc_x, anchor_doc_y);
                let page = self.pages.get(page_idx)?;
                (
                    page_idx,
                    anchor_doc_x - page.offset_x,
                    anchor_doc_y - page.offset_y,
                    viewport_y_fraction,
                )
            };
        let page = self.pages.get(page_idx)?;
        let pdf_point = Self::view_point_to_pdf(
            page.bounds,
            self.rotation,
            mupdf::Point {
                x: center_x,
                y: anchor_y,
            },
        );
        (pdf_point.x.is_finite() && pdf_point.y.is_finite()).then_some(TranslationViewportAnchor {
            page_index: page_idx,
            pdf_x: pdf_point.x,
            pdf_y: pdf_point.y,
            viewport_y_fraction,
            mode,
        })
    }

    pub(super) fn emit_live_translation_viewport_changed(
        &self,
        mode: SynchronizedScrollMode,
        cx: &mut Context<Self>,
    ) {
        if self.translation.suppress_linked_events {
            return;
        }
        let Some(session_id) = self.translation.pair_session_id else {
            return;
        };
        let Some(anchor) = self.live_translation_viewport_anchor(mode) else {
            return;
        };
        cx.emit(DocumentViewEvent::TranslationViewportChanged { session_id, anchor });
    }

    pub fn apply_live_translation_viewport_anchor(
        &mut self,
        session_id: DocumentTranslationId,
        anchor: TranslationViewportAnchor,
        cx: &mut Context<Self>,
    ) {
        if self.translation.pair_session_id != Some(session_id)
            || self.translation.view_role == TranslationViewRole::None
            || self.format != DocumentFormat::Pdf
            || self.capabilities.reflow
            || self.pages.get(anchor.page_index).is_none()
        {
            return;
        }
        self.translation.position_sync_ready = true;
        if !self.translation_viewport_geometry_ready() {
            self.translation.pending_viewport_anchor = Some(anchor);
            return;
        }
        self.translation.pending_viewport_anchor = None;
        let page = &self.pages[anchor.page_index];
        let point = Self::pdf_point_to_view(
            page.bounds,
            self.rotation,
            mupdf::Point {
                x: anchor.pdf_x,
                y: anchor.pdf_y,
            },
        );
        let scroll_x_doc = page.offset_x + point.x - self.max_page_width / 2.0;
        let viewport_y_fraction = anchor.viewport_y_fraction.clamp(0.0, 1.0);
        let scroll_y_doc = page.offset_y + point.y
            - self.viewport_height / self.effective_scale() * viewport_y_fraction;
        let was_suppressed = self.translation.suppress_linked_events;
        self.translation.suppress_linked_events = true;
        if self.is_presentation_mode() {
            self.set_presentation_page(anchor.page_index, cx);
        } else {
            match anchor.mode {
                SynchronizedScrollMode::Immediate => {
                    self.set_scroll_doc(scroll_x_doc, scroll_y_doc, cx);
                }
                SynchronizedScrollMode::Target => {
                    self.set_scroll_doc_target(scroll_x_doc, scroll_y_doc, cx);
                }
            }
        }
        self.translation.suppress_linked_events = was_suppressed;
    }

    pub(super) fn apply_pending_live_translation_viewport_anchor(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.translation.pair_session_id else {
            self.translation.pending_viewport_anchor = None;
            return;
        };
        let Some(anchor) = self.translation.pending_viewport_anchor.take() else {
            return;
        };
        self.apply_live_translation_viewport_anchor(session_id, anchor, cx);
    }

    pub(super) fn viewport_center_pdf_anchor(&self) -> Option<(usize, mupdf::Point)> {
        if self.pages.is_empty() {
            return None;
        }
        let scale = self.effective_scale();
        let center_y = self.target_scroll_y_doc + self.viewport_height / (2.0 * scale);
        let center_x = self.max_page_width / 2.0 + self.target_scroll_x_doc;
        let page_idx = self.page_index_for_doc_point(center_x, center_y);
        let page = self.pages.get(page_idx)?;
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let point = mupdf::Point {
            x: (center_x - page.offset_x).clamp(0.0, page_width),
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
            let scale = self.effective_scale();
            self.scroll_x_doc = page.offset_x + point.x - self.max_page_width / 2.0;
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
        session_id: DocumentTranslationId,
        region_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.translation.view_role != TranslationViewRole::SourceMirror
            || self.translation.pair_session_id != Some(session_id)
            || !self.translation.pair_active
            || (!self.translation.overlay_enabled && region_id.is_some())
        {
            return;
        }
        if self.translation.external_hovered_region != region_id {
            self.translation.external_hovered_region = region_id;
            cx.notify();
        }
    }

    fn publish_hovered_translation_region(
        &mut self,
        region_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.translation.view_role != TranslationViewRole::Translation
            || !self.translation.pair_active
            || (!self.translation.overlay_enabled && region_id.is_some())
            || self.translation.hovered_region == region_id
        {
            return;
        }
        self.translation.hover_tools_bounds = None;
        self.translation.hovered_region = region_id.clone();
        if !self.translation.suppress_linked_events
            && let Some(session_id) = self.translation.pair_session_id
        {
            cx.emit(DocumentViewEvent::TranslationRegionHovered {
                session_id,
                region_id,
            });
        }
        cx.notify();
    }

    pub(super) fn toggle_translation_overlay(&mut self, cx: &mut Context<Self>) {
        if self.translation.view_role != TranslationViewRole::Translation {
            return;
        }
        self.translation.overlay_enabled = !self.translation.overlay_enabled;
        if !self.translation.overlay_enabled {
            self.clear_translation_hover_state(true, cx);
        }
        if let Some(session_id) = self.translation.pair_session_id {
            cx.emit(DocumentViewEvent::TranslationOverlayVisibilityChanged {
                session_id,
                enabled: self.translation.overlay_enabled,
            });
        }
        cx.notify();
    }

    pub fn apply_live_translation_overlay_visibility(
        &mut self,
        session_id: DocumentTranslationId,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if self.translation.view_role != TranslationViewRole::SourceMirror
            || self.translation.pair_session_id != Some(session_id)
            || self.translation.overlay_enabled == enabled
        {
            return;
        }
        self.translation.overlay_enabled = enabled;
        if !enabled {
            self.clear_translation_hover_state(true, cx);
        }
        cx.notify();
    }

    pub(super) fn cancel_translation_hover_clear(&mut self) {
        self.translation.hover_clear_generation =
            self.translation.hover_clear_generation.wrapping_add(1);
        self.translation.hover_clear_task = None;
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
        if self.translation.view_role != TranslationViewRole::Translation
            || !self.translation.pair_active
            || self.translation.hover_tools_hovered
            || self.translation.hovered_region.is_none()
            || self.translation.hover_clear_task.is_some()
        {
            return;
        }
        self.translation.hover_clear_generation =
            self.translation.hover_clear_generation.wrapping_add(1);
        let generation = self.translation.hover_clear_generation;
        self.translation.hover_clear_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(TRANSLATION_HOVER_CLEAR_GRACE)
                .await;
            _ = this.update(cx, |this, cx| {
                if this.translation.hover_clear_generation != generation
                    || this.translation.hover_tools_hovered
                {
                    return;
                }
                this.translation.hover_clear_task = None;
                this.publish_hovered_translation_region(None, cx);
            });
        }));
    }

    pub(super) fn set_translation_hover_tools_hovered(
        &mut self,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if self.translation.hover_tools_hovered == hovered {
            return;
        }
        self.translation.hover_tools_hovered = hovered;
        if hovered {
            self.cancel_translation_hover_clear();
        } else {
            self.schedule_translation_hover_clear(cx);
        }
    }

    pub(super) fn record_translation_hover_tools_bounds(
        &mut self,
        region_id: String,
        bounds: Bounds<Pixels>,
    ) {
        if self.translation.hovered_region.as_deref() == Some(region_id.as_str()) {
            self.translation.hover_tools_bounds = Some((region_id, bounds));
        }
    }

    pub(super) fn translation_hover_interaction_contains(
        &self,
        position: GpuiPoint<Pixels>,
    ) -> bool {
        let Some(region_id) = self.translation.hovered_region.as_deref() else {
            return false;
        };
        let Some((tools_region_id, tools_bounds)) = self.translation.hover_tools_bounds.as_ref()
        else {
            return false;
        };
        if tools_region_id != region_id {
            return false;
        }
        let now = Instant::now();
        let highlight_bounds = self
            .translation
            .hover_animation
            .as_ref()
            .filter(|animation| animation.region_id == region_id)
            .map(|animation| animation.bounds(now))
            .or_else(|| {
                self.translation_hover_region_highlights(region_id)
                    .into_iter()
                    .reduce(|left, right| left.union(&right))
            });
        translation_hover_interaction_bounds(highlight_bounds, *tools_bounds).contains(&position)
    }

    pub(super) fn clear_translation_hover_state(
        &mut self,
        reset_animation: bool,
        cx: &mut Context<Self>,
    ) {
        self.cancel_translation_hover_clear();
        self.translation.hover_tools_hovered = false;
        self.translation.hover_tools_bounds = None;
        self.publish_hovered_translation_region(None, cx);
        self.translation.external_hovered_region = None;
        if reset_animation {
            self.translation.hover_animation = None;
        }
    }
}
