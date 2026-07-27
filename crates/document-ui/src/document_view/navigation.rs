use super::*;

impl DocumentView {
    pub(super) fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.zoom).abs() <= f32::EPSILON {
            return;
        }

        let old_scale = self.effective_scale();
        let center_doc = self.scroll_y_doc + self.viewport_height / (2.0 * old_scale);

        self.zoom = zoom;

        let new_scale = self.effective_scale();
        self.scroll_y_doc = center_doc - self.viewport_height / (2.0 * new_scale);
        self.target_scroll_x_doc = self.scroll_x_doc;
        self.target_scroll_y_doc = self.scroll_y_doc;
        self.clamp_scroll();
        self.hovered_link = None;
        self.pending_link = None;
        self.schedule_position_save(cx);
        self.emit_linked_viewport(cx);
        cx.notify();
    }

    pub(super) fn zoom_in(&mut self, cx: &mut Context<Self>) {
        let zoom_step = cx.global::<SettingsStore>().settings.behavior.zoom_step;
        if self.grid_overview_open {
            self.set_grid_overview_zoom(self.grid_overview_zoom * zoom_step, cx);
            return;
        }
        if self.is_presentation_mode() {
            return;
        }

        self.set_zoom(self.zoom * zoom_step, cx);
    }

    pub(super) fn zoom_out(&mut self, cx: &mut Context<Self>) {
        let zoom_step = cx.global::<SettingsStore>().settings.behavior.zoom_step;
        if self.grid_overview_open {
            self.set_grid_overview_zoom(self.grid_overview_zoom / zoom_step, cx);
            return;
        }
        if self.is_presentation_mode() {
            return;
        }

        self.set_zoom(self.zoom / zoom_step, cx);
    }

    pub(super) fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        if self.is_presentation_mode() {
            return;
        }

        let default_zoom = cx.global::<SettingsStore>().settings.behavior.default_zoom;
        self.set_zoom(default_zoom, cx);
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
        let mut current = 0;

        for (idx, page) in self.pages.iter().enumerate() {
            if center_doc >= page.offset_y {
                current = idx;
            } else {
                break;
            }
        }

        current
    }

    pub(super) fn page_index_for_doc_y(&self, doc_y: f32) -> usize {
        let mut current = 0;
        for (idx, page) in self.pages.iter().enumerate() {
            if doc_y >= page.offset_y {
                current = idx;
            } else {
                break;
            }
        }

        current
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

    pub(super) fn page_counter_label(current_page: usize, page_count: usize) -> String {
        format!("{current_page} / {page_count}")
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
        let target_page = page_idx.min(self.last_page_index());
        if self.is_presentation_mode() {
            if self.set_presentation_page(target_page, cx) {
                self.mark_manual_navigation();
            }
            return;
        }

        let target = self
            .pages
            .get(target_page)
            .map(|page| page.offset_y)
            .unwrap_or_else(|| self.max_scroll_y_doc());

        if self.set_scroll_doc_target(self.target_scroll_x_doc, target, cx) {
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

    fn set_grid_overview_cursor(
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

        self.navigate_to_page_start(self.current_page_index() + 1, cx);
    }

    pub(super) fn previous_page(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_by(-1.0, 1.0, cx);
            return;
        }

        self.navigate_to_page_start(self.current_page_index().saturating_sub(1), cx);
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
        if self.set_scroll_doc_target(
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
        if self.set_scroll_doc_target(
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
                self.set_zoom(self.zoom * factor, cx);
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
            if self.set_scroll_doc_target(
                self.target_scroll_x_doc - delta_x / scale,
                self.target_scroll_y_doc - delta_y / scale,
                cx,
            ) {
                self.mark_manual_navigation();
            }
        }
    }
}
