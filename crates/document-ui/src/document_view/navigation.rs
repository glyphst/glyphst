use super::*;

#[derive(Clone, Copy)]
struct ZoomFitBounds {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

pub(super) fn zoom_for_fit_extent(
    viewport_extent: f32,
    base_scale: f32,
    content_extent: f32,
) -> Option<f32> {
    if !viewport_extent.is_finite()
        || !base_scale.is_finite()
        || !content_extent.is_finite()
        || viewport_extent <= 0.0
        || base_scale <= 0.0
        || content_extent <= 0.0
    {
        return None;
    }

    let zoom = viewport_extent / (base_scale * content_extent);
    zoom.is_finite().then_some(zoom)
}

impl DocumentView {
    pub(super) fn zoom_anchor_at_viewport(&self, viewport: [f32; 2]) -> ZoomAnchor {
        let scale = self.effective_scale();
        let horizontal_inset = (self.viewport_width - self.max_page_width * scale) / 2.0;
        ZoomAnchor {
            document: [
                self.scroll_x_doc + (viewport[0] - horizontal_inset) / scale,
                self.scroll_y_doc + viewport[1] / scale,
            ],
            viewport,
        }
    }

    pub(super) fn apply_zoom_anchor(&mut self, zoom: f32, anchor: ZoomAnchor) {
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let scale = self.effective_scale();
        let horizontal_inset = (self.viewport_width - self.max_page_width * scale) / 2.0;
        self.scroll_x_doc = anchor.document[0] - (anchor.viewport[0] - horizontal_inset) / scale;
        self.scroll_y_doc = anchor.document[1] - anchor.viewport[1] / scale;
        self.target_scroll_x_doc = self.scroll_x_doc;
        self.target_scroll_y_doc = self.scroll_y_doc;
        self.clamp_scroll();
    }

    pub(super) fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.zoom).abs() <= f32::EPSILON
            && (zoom - self.target_zoom).abs() <= f32::EPSILON
        {
            return;
        }
        let anchor =
            self.zoom_anchor_at_viewport([self.viewport_width / 2.0, self.viewport_height / 2.0]);
        self.apply_zoom_anchor(zoom, anchor);
        self.target_zoom = zoom;
        self.zoom_anchor = None;
        self.last_animation_frame = None;
        self.mark_native_gpu_zoom_motion(cx);
        self.hovered_link = None;
        self.pending_link = None;
        self.schedule_position_save(cx);
        cx.notify();
    }

    pub(super) fn set_zoom_target(
        &mut self,
        zoom: f32,
        viewport_anchor: [f32; 2],
        cx: &mut Context<Self>,
    ) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.target_zoom).abs() <= f32::EPSILON {
            return;
        }
        let viewport_anchor = [
            viewport_anchor[0].clamp(0.0, self.viewport_width.max(0.0)),
            viewport_anchor[1].clamp(0.0, self.viewport_height.max(0.0)),
        ];
        self.target_zoom = zoom;
        self.zoom_anchor = Some(self.zoom_anchor_at_viewport(viewport_anchor));
        self.last_animation_frame = None;
        self.mark_native_gpu_zoom_motion(cx);
        self.hovered_link = None;
        self.pending_link = None;
        self.schedule_position_save(cx);
        cx.notify();
    }

    fn cancel_zoom_popup_hide(&mut self) {
        self.zoom_popup_generation = self.zoom_popup_generation.wrapping_add(1);
        self.zoom_popup_hide_task = None;
    }

    fn schedule_zoom_popup_hide(&mut self, cx: &mut Context<Self>) {
        self.cancel_zoom_popup_hide();
        if !self.zoom_popup_visible || self.zoom_popup_hovered {
            return;
        }

        let generation = self.zoom_popup_generation;
        self.zoom_popup_hide_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(glyphst_ui::TRANSIENT_FEEDBACK_DURATION)
                .await;
            _ = this.update(cx, |this, cx| {
                if this.zoom_popup_generation != generation || this.zoom_popup_hovered {
                    return;
                }
                this.zoom_popup_hide_task = None;
                this.zoom_popup_visible = false;
                this.zoom_popup_hovered = false;
                cx.notify();
            });
        }));
    }

    fn show_zoom_popup(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open || self.is_presentation_mode() {
            return;
        }
        self.zoom_popup_visible = true;
        self.schedule_zoom_popup_hide(cx);
        cx.notify();
    }

    pub(super) fn dismiss_zoom_popup(&mut self) {
        if !self.zoom_popup_visible
            && !self.zoom_popup_hovered
            && self.zoom_popup_hide_task.is_none()
        {
            return;
        }
        self.zoom_popup_visible = false;
        self.zoom_popup_hovered = false;
        self.cancel_zoom_popup_hide();
    }

    pub(super) fn set_zoom_popup_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.zoom_popup_hovered == hovered {
            return;
        }
        self.zoom_popup_hovered = hovered;
        if hovered {
            self.cancel_zoom_popup_hide();
        } else {
            self.schedule_zoom_popup_hide(cx);
        }
    }

    fn current_zoom_fit_bounds(&self) -> Option<ZoomFitBounds> {
        let current_page = self.current_page_index();
        let range = self.spread_page_range(current_page);
        let first_page = self.pages.get(range.start)?;
        let mut top = f32::INFINITY;
        let mut bottom = f32::NEG_INFINITY;
        for page_idx in range {
            let page = self.pages.get(page_idx)?;
            let (_, height) = self.page_dimensions(page_idx);
            top = top.min(page.offset_y);
            bottom = bottom.max(page.offset_y + height);
        }

        let (left, width) = if self.is_two_column_layout() {
            (0.0, self.max_page_width)
        } else {
            let (width, _) = self.page_dimensions(current_page);
            (first_page.offset_x, width)
        };
        let height = bottom - top;
        (left.is_finite()
            && top.is_finite()
            && width.is_finite()
            && height.is_finite()
            && width > 0.0
            && height > 0.0)
            .then_some(ZoomFitBounds {
                left,
                top,
                width,
                height,
            })
    }

    pub(super) fn fit_to_width(&mut self, cx: &mut Context<Self>) {
        if self.capabilities.reflow || self.grid_overview_open || self.is_presentation_mode() {
            return;
        }
        let Some(bounds) = self.current_zoom_fit_bounds() else {
            return;
        };
        let Some(zoom) = zoom_for_fit_extent(self.viewport_width, self.base_scale, bounds.width)
        else {
            return;
        };

        self.set_zoom(zoom, cx);
        let centered_x = bounds.left + bounds.width / 2.0 - self.max_page_width / 2.0;
        self.set_scroll_doc(centered_x, self.scroll_y_doc, cx);
        self.show_zoom_popup(cx);
    }

    pub(super) fn fit_to_height(&mut self, cx: &mut Context<Self>) {
        if self.capabilities.reflow || self.grid_overview_open || self.is_presentation_mode() {
            return;
        }
        let Some(bounds) = self.current_zoom_fit_bounds() else {
            return;
        };
        let Some(zoom) = zoom_for_fit_extent(self.viewport_height, self.base_scale, bounds.height)
        else {
            return;
        };

        self.set_zoom(zoom, cx);
        let centered_x = bounds.left + bounds.width / 2.0 - self.max_page_width / 2.0;
        self.set_scroll_doc(centered_x, bounds.top, cx);
        self.show_zoom_popup(cx);
    }

    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        let zoom_step = cx.global::<SettingsStore>().settings.behavior.zoom_step;
        if self.grid_overview_open {
            self.set_grid_overview_zoom(self.grid_overview_zoom * zoom_step, cx);
            return;
        }
        if self.is_presentation_mode() {
            return;
        }

        self.set_zoom(self.target_zoom * zoom_step, cx);
        self.show_zoom_popup(cx);
    }

    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        let zoom_step = cx.global::<SettingsStore>().settings.behavior.zoom_step;
        if self.grid_overview_open {
            self.set_grid_overview_zoom(self.grid_overview_zoom / zoom_step, cx);
            return;
        }
        if self.is_presentation_mode() {
            return;
        }

        self.set_zoom(self.target_zoom / zoom_step, cx);
        self.show_zoom_popup(cx);
    }

    pub fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open || self.is_presentation_mode() {
            return;
        }

        let default_zoom = cx.global::<SettingsStore>().settings.behavior.default_zoom;
        self.set_zoom(default_zoom, cx);
        self.show_zoom_popup(cx);
    }

    fn set_grid_overview_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(GRID_OVERVIEW_MIN_ZOOM, GRID_OVERVIEW_MAX_ZOOM);
        if (zoom - self.grid_overview_zoom).abs() <= f32::EPSILON {
            return;
        }

        self.grid_overview_zoom = zoom;
        self.update_grid_overview_cols(self.grid_overview_viewport_width, cx);
        cx.notify();
    }

    pub(super) fn page_index_for_scroll_y(&self, scroll_y_doc: f32) -> usize {
        let scale = self.effective_scale();
        let center_doc = scroll_y_doc + self.viewport_height / (2.0 * scale);
        self.page_index_for_doc_y(center_doc)
    }

    pub(super) fn page_index_for_doc_y(&self, doc_y: f32) -> usize {
        let current = self
            .pages
            .partition_point(|page| page.offset_y <= doc_y)
            .saturating_sub(1)
            .min(self.last_page_index());
        self.spread_start_page(current)
    }

    pub fn is_presentation_mode(&self) -> bool {
        self.presentation_page.is_some()
    }

    pub(super) fn presentation_page_index(&self) -> Option<usize> {
        self.presentation_page
            .map(|page_idx| page_idx.min(self.last_page_index()))
    }

    pub(super) fn page_scroll_y_doc(&self, page_idx: usize) -> f32 {
        self.pages
            .get(page_idx.min(self.last_page_index()))
            .map(|page| page.offset_y)
            .unwrap_or(0.0)
    }

    pub(super) fn current_page_index(&self) -> usize {
        if let Some(page_idx) = self.presentation_page_index() {
            return page_idx;
        }

        self.page_index_for_scroll_y(self.scroll_y_doc)
    }

    pub(crate) fn current_page(&self) -> usize {
        self.current_page_index() + 1
    }

    pub(crate) fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub(super) fn current_page_range(&self) -> Range<usize> {
        if let Some(page_idx) = self.presentation_page_index() {
            return page_idx..(page_idx + 1).min(self.pages.len());
        }
        self.spread_page_range(self.current_page_index())
    }

    pub(super) fn page_counter_label(page_range: Range<usize>, page_count: usize) -> String {
        let start = page_range.start + 1;
        if page_range.end > page_range.start + 1 {
            format!("{start}–{} / {page_count}", page_range.end)
        } else {
            format!("{start} / {page_count}")
        }
    }

    pub(super) fn last_page_index(&self) -> usize {
        self.pages.len().saturating_sub(1)
    }

    pub(super) fn set_presentation_page(
        &mut self,
        page_idx: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let page_idx = page_idx.min(self.last_page_index());
        if self.presentation_page == Some(page_idx) {
            return false;
        }

        self.presentation_page = Some(page_idx);
        self.hovered_link = None;
        self.hovered_note = None;
        self.hovered_note_position = None;
        self.pending_link = None;
        self.schedule_position_save(cx);
        self.emit_live_translation_viewport_changed(SynchronizedScrollMode::Immediate, cx);
        cx.notify();
        true
    }

    fn navigate_presentation_by_direction(&mut self, direction: f32, cx: &mut Context<Self>) {
        if direction > 0.0 {
            if self.set_presentation_page(self.current_page_index() + 1, cx) {
                self.mark_manual_navigation();
            }
        } else if direction < 0.0
            && self.set_presentation_page(self.current_page_index().saturating_sub(1), cx)
        {
            self.mark_manual_navigation();
        }
    }

    fn navigate_to_page_start(&mut self, page_idx: usize, cx: &mut Context<Self>) {
        if self.is_presentation_mode() {
            let target_page = page_idx.min(self.last_page_index());
            if self.set_presentation_page(target_page, cx) {
                self.mark_manual_navigation();
            }
            return;
        }

        let target_page = self.spread_start_page(page_idx);

        let target = self
            .pages
            .get(target_page)
            .map(|page| page.offset_y)
            .unwrap_or_else(|| self.max_scroll_y_doc());

        if self.set_scroll_doc_target_synchronized(self.target_scroll_x_doc, target, cx) {
            self.mark_manual_navigation();
        }
    }

    fn grid_overview_cursor_row(&self) -> usize {
        self.grid_overview_cursor / self.grid_overview_cols.max(1)
    }

    pub(super) fn scroll_grid_overview_cursor(&self, strategy: ScrollStrategy) {
        self.grid_overview_scroll
            .scroll_to_item(self.grid_overview_cursor_row(), strategy);
    }

    pub(super) fn set_grid_overview_cursor(
        &mut self,
        page_idx: usize,
        strategy: ScrollStrategy,
        cx: &mut Context<Self>,
    ) {
        self.grid_overview_cursor = page_idx.min(self.last_page_index());
        self.scroll_grid_overview_cursor(strategy);
        cx.notify();
    }

    fn move_grid_overview_cursor(
        &mut self,
        delta: isize,
        strategy: ScrollStrategy,
        cx: &mut Context<Self>,
    ) {
        let target = if delta < 0 {
            self.grid_overview_cursor
                .saturating_sub(delta.unsigned_abs())
        } else {
            self.grid_overview_cursor.saturating_add(delta as usize)
        };
        self.set_grid_overview_cursor(target, strategy, cx);
    }

    fn scroll_grid_overview_by(&mut self, direction: f32, fraction: f32, cx: &mut Context<Self>) {
        let scroll_handle = self.grid_overview_scroll.0.borrow().base_handle.clone();
        let offset = scroll_handle.offset();
        let max_offset = scroll_handle.max_offset();
        let delta = scroll_handle.bounds().size.height.as_f32() * fraction;
        let max_y = max_offset.y.as_f32().max(0.0);
        if max_y <= 0.0 || delta <= 0.0 {
            return;
        }

        let target_y = (offset.y.as_f32() - direction * delta).clamp(-max_y, 0.0);
        if (target_y - offset.y.as_f32()).abs() <= f32::EPSILON {
            return;
        }

        scroll_handle.set_offset(point(offset.x, px(target_y)));
        cx.notify();
    }

    fn scroll_grid_overview_to_top(&mut self, cx: &mut Context<Self>) {
        self.grid_overview_scroll
            .scroll_to_item_strict(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn scroll_grid_overview_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.grid_overview_scroll.scroll_to_bottom();
        cx.notify();
    }

    pub(super) fn confirm_grid_overview_selection(&mut self, cx: &mut Context<Self>) {
        let page_idx = self.grid_overview_cursor.min(self.last_page_index());
        self.navigate_to_pdf_destination(page_idx, None, None, cx);
        self.set_grid_overview_open(false, cx);
    }

    pub(super) fn grid_cursor_up(&mut self, cx: &mut Context<Self>) {
        self.move_grid_overview_cursor(
            -(self.grid_overview_cols.max(1) as isize),
            ScrollStrategy::Center,
            cx,
        );
    }

    pub(super) fn grid_cursor_down(&mut self, cx: &mut Context<Self>) {
        self.move_grid_overview_cursor(
            self.grid_overview_cols.max(1) as isize,
            ScrollStrategy::Center,
            cx,
        );
    }

    pub(super) fn grid_cursor_left(&mut self, cx: &mut Context<Self>) {
        self.move_grid_overview_cursor(-1, ScrollStrategy::Center, cx);
    }

    pub(super) fn grid_cursor_right(&mut self, cx: &mut Context<Self>) {
        self.move_grid_overview_cursor(1, ScrollStrategy::Center, cx);
    }

    pub(super) fn next_page(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_by(1.0, 1.0, cx);
            return;
        }

        let step = if self.is_two_column_layout() && !self.is_presentation_mode() {
            2
        } else {
            1
        };
        self.navigate_to_page_start(self.current_page_index().saturating_add(step), cx);
    }

    pub(super) fn previous_page(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_by(-1.0, 1.0, cx);
            return;
        }

        let step = if self.is_two_column_layout() && !self.is_presentation_mode() {
            2
        } else {
            1
        };
        self.navigate_to_page_start(self.current_page_index().saturating_sub(step), cx);
    }

    pub(super) fn go_to_beginning(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_to_top(cx);
            return;
        }
        if self.is_presentation_mode() {
            if self.set_presentation_page(0, cx) {
                self.mark_manual_navigation();
            }
            return;
        }

        if self.set_scroll_doc_target(self.target_scroll_x_doc, 0.0, cx) {
            self.mark_manual_navigation();
        }
    }

    pub(super) fn go_to_end(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_to_bottom(cx);
            return;
        }
        if self.is_presentation_mode() {
            if self.set_presentation_page(self.last_page_index(), cx) {
                self.mark_manual_navigation();
            }
            return;
        }

        if self.set_scroll_doc_target(self.target_scroll_x_doc, self.max_scroll_y_doc(), cx) {
            self.mark_manual_navigation();
        }
    }

    pub(super) fn next_half_page(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_by(1.0, 0.5, cx);
            return;
        }

        self.scroll_half_page(1.0, cx);
    }

    pub(super) fn previous_half_page(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_by(-1.0, 0.5, cx);
            return;
        }

        self.scroll_half_page(-1.0, cx);
    }

    fn scroll_half_page(&mut self, direction: f32, cx: &mut Context<Self>) {
        if self.is_presentation_mode() {
            self.navigate_presentation_by_direction(direction, cx);
            return;
        }

        let delta = self.viewport_height / (2.0 * self.effective_scale());
        if self.set_scroll_doc_target_synchronized(
            self.target_scroll_x_doc,
            self.target_scroll_y_doc + direction * delta,
            cx,
        ) {
            self.mark_manual_navigation();
        }
    }

    pub(super) fn scroll_line(&mut self, direction: f32, cx: &mut Context<Self>) {
        if self.is_presentation_mode() {
            self.navigate_presentation_by_direction(direction, cx);
            return;
        }

        let delta = SCROLL_LINE_STEP_PX / self.effective_scale();
        if self.set_scroll_doc_target_synchronized(
            self.target_scroll_x_doc,
            self.target_scroll_y_doc + direction * delta,
            cx,
        ) {
            self.mark_manual_navigation();
        }
    }

    pub(super) fn scroll_wheel_requests_zoom(event: &ScrollWheelEvent) -> bool {
        event.modifiers.control
    }

    pub(super) fn handle_scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if Self::scroll_wheel_requests_zoom(event) {
            let zoom_delta = match event.delta {
                ScrollDelta::Pixels(pixels) => pixels.y.as_f32() / SCROLL_LINE_MULTIPLIER,
                ScrollDelta::Lines(lines) => lines.y,
            };
            if zoom_delta == 0.0 {
                return;
            }

            let factor = cx
                .global::<SettingsStore>()
                .settings
                .behavior
                .zoom_step
                .powf(zoom_delta * CTRL_SCROLL_ZOOM_STEP_MULTIPLIER);
            if self.grid_overview_open {
                self.set_grid_overview_zoom(self.grid_overview_zoom * factor, cx);
            } else if !self.is_presentation_mode() {
                self.set_zoom_target(
                    self.target_zoom * factor,
                    [
                        (event.position.x - self.viewport_origin.x).as_f32(),
                        (event.position.y - self.viewport_origin.y).as_f32(),
                    ],
                    cx,
                );
                self.show_zoom_popup(cx);
            }
        } else {
            let (delta_x, delta_y) = match event.delta {
                ScrollDelta::Pixels(pixels) => (pixels.x.as_f32(), pixels.y.as_f32()),
                ScrollDelta::Lines(lines) => (
                    lines.x * SCROLL_LINE_MULTIPLIER,
                    lines.y * SCROLL_LINE_MULTIPLIER,
                ),
            };
            if delta_x == 0.0 && delta_y == 0.0 {
                return;
            }
            if self.is_presentation_mode() {
                self.navigate_presentation_by_direction(-delta_y, cx);
                return;
            }

            let scale = self.effective_scale();
            if self.set_scroll_doc_target_synchronized(
                self.target_scroll_x_doc - delta_x / scale,
                self.target_scroll_y_doc - delta_y / scale,
                cx,
            ) {
                self.mark_manual_navigation();
            }
        }
    }
}
