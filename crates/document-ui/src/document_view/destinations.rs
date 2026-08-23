use super::*;

impl DocumentView {
    pub(super) fn transformed_destination_axes(
        page_bounds: Rect,
        rotation: DocumentRotation,
        x: Option<f64>,
        y: Option<f64>,
    ) -> Option<(Option<f32>, Option<f32>)> {
        let matrix = Self::page_to_view_matrix(page_bounds, rotation);
        let mut view_x = None;
        let mut view_y = None;
        if let Some(x) = x {
            Self::assign_transformed_destination_axis(
                f64::from(matrix.a),
                f64::from(matrix.b),
                x,
                f64::from(matrix.e),
                f64::from(matrix.f),
                &mut view_x,
                &mut view_y,
            )?;
        }
        if let Some(y) = y {
            Self::assign_transformed_destination_axis(
                f64::from(matrix.c),
                f64::from(matrix.d),
                y,
                f64::from(matrix.e),
                f64::from(matrix.f),
                &mut view_x,
                &mut view_y,
            )?;
        }
        Some((view_x, view_y))
    }

    #[allow(clippy::too_many_arguments)]
    fn assign_transformed_destination_axis(
        x_coefficient: f64,
        y_coefficient: f64,
        value: f64,
        x_translation: f64,
        y_translation: f64,
        view_x: &mut Option<f32>,
        view_y: &mut Option<f32>,
    ) -> Option<()> {
        const EPSILON: f64 = 1.0e-12;
        let maps_x = x_coefficient.abs() > EPSILON;
        let maps_y = y_coefficient.abs() > EPSILON;
        let transformed = match (maps_x, maps_y) {
            (true, false) => x_coefficient.mul_add(value, x_translation),
            (false, true) => y_coefficient.mul_add(value, y_translation),
            _ => return None,
        };
        let transformed = transformed as f32;
        if !transformed.is_finite() {
            return None;
        }
        let target = if maps_x { view_x } else { view_y };
        if target.replace(transformed).is_some() {
            return None;
        }
        Some(())
    }

    fn destination_zoom_for_extent(&self, viewport: f32, extent: f32) -> Option<f32> {
        super::navigation::zoom_for_fit_extent(viewport, self.base_scale, extent)
    }

    fn destination_left_scroll(&self, page: &DocumentPage, x: f32) -> f32 {
        page.offset_x + x + self.viewport_width / (2.0 * self.effective_scale())
            - self.max_page_width / 2.0
    }

    fn destination_center_scroll(&self, page: &DocumentPage, x: f32) -> f32 {
        page.offset_x + x - self.max_page_width / 2.0
    }

    fn navigate_to_gpu_destination(
        &mut self,
        page_idx: usize,
        view: &GpuDestinationView,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = self.pages.get(page_idx).cloned() else {
            return;
        };

        if self.is_presentation_mode() {
            let current_position = self.current_jump_position();
            Self::push_jump_position(&mut self.jump_back, current_position);
            self.jump_forward.clear();
            self.mark_manual_navigation();
            self.set_presentation_page(page_idx, cx);
            return;
        }

        let current_page_idx = self.page_index_for_scroll_y(self.scroll_y_doc);
        let retained_top = self
            .pages
            .get(current_page_idx)
            .map_or(0.0, |current| self.scroll_y_doc - current.offset_y);
        let retained_x = self.scroll_x_doc;
        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        self.jump_forward.clear();
        self.mark_manual_navigation();

        let (page_width, page_height) = self.page_dimensions(page_idx);
        let mut target_x = retained_x;
        let mut target_y = page.offset_y + retained_top;
        match view {
            GpuDestinationView::Xyz { x, y, zoom } => {
                let Some((x, y)) =
                    Self::transformed_destination_axes(page.bounds, self.rotation, *x, *y)
                else {
                    return;
                };
                if let Some(zoom) = zoom {
                    let relative_zoom = *zoom / self.base_scale.max(0.001);
                    self.set_zoom(relative_zoom, cx);
                }
                if let Some(x) = x {
                    target_x = self.destination_left_scroll(&page, x);
                }
                if let Some(y) = y {
                    target_y = page.offset_y + y;
                }
            }
            GpuDestinationView::FitWidth { top } => {
                let Some((view_x, view_y)) =
                    Self::transformed_destination_axes(page.bounds, self.rotation, None, *top)
                else {
                    return;
                };
                if self.rotation.swaps_width_and_height() {
                    if let Some(zoom) =
                        self.destination_zoom_for_extent(self.viewport_height, page_height)
                    {
                        self.set_zoom(zoom, cx);
                    }
                    target_x = view_x
                        .map(|left| self.destination_left_scroll(&page, left))
                        .unwrap_or(retained_x);
                    target_y = page.offset_y;
                } else {
                    if let Some(zoom) =
                        self.destination_zoom_for_extent(self.viewport_width, page_width)
                    {
                        self.set_zoom(zoom, cx);
                    }
                    target_x = self.destination_center_scroll(&page, page_width / 2.0);
                    target_y = view_y
                        .map(|top| page.offset_y + top)
                        .unwrap_or(page.offset_y + retained_top);
                }
            }
            GpuDestinationView::FitHeight { left } => {
                let Some((view_x, view_y)) =
                    Self::transformed_destination_axes(page.bounds, self.rotation, *left, None)
                else {
                    return;
                };
                if self.rotation.swaps_width_and_height() {
                    if let Some(zoom) =
                        self.destination_zoom_for_extent(self.viewport_width, page_width)
                    {
                        self.set_zoom(zoom, cx);
                    }
                    target_x = self.destination_center_scroll(&page, page_width / 2.0);
                    target_y = view_y
                        .map(|top| page.offset_y + top)
                        .unwrap_or(page.offset_y + retained_top);
                } else {
                    if let Some(zoom) =
                        self.destination_zoom_for_extent(self.viewport_height, page_height)
                    {
                        self.set_zoom(zoom, cx);
                    }
                    target_x = view_x
                        .map(|left| self.destination_left_scroll(&page, left))
                        .unwrap_or(retained_x);
                    target_y = page.offset_y;
                }
            }
            GpuDestinationView::FitPage => {
                let zoom = self
                    .destination_zoom_for_extent(self.viewport_width, page_width)
                    .zip(self.destination_zoom_for_extent(self.viewport_height, page_height))
                    .map(|(width, height)| width.min(height));
                if let Some(zoom) = zoom {
                    self.set_zoom(zoom, cx);
                }
                target_x = self.destination_center_scroll(&page, page_width / 2.0);
                target_y = page.offset_y;
            }
            GpuDestinationView::FitBoundingBox { content_bounds } => {
                let Some(rect) = content_bounds.and_then(|bounds| {
                    Self::gpu_destination_rect(page.bounds, self.rotation, bounds)
                }) else {
                    return;
                };
                let zoom = self
                    .destination_zoom_for_extent(self.viewport_width, rect.width())
                    .zip(self.destination_zoom_for_extent(self.viewport_height, rect.height()))
                    .map(|(width, height)| width.min(height));
                if let Some(zoom) = zoom {
                    self.set_zoom(zoom, cx);
                }
                target_x = self.destination_center_scroll(&page, (rect.x0 + rect.x1) / 2.0);
                let viewport_height_doc = self.viewport_height / self.effective_scale();
                target_y = page.offset_y + (rect.y0 + rect.y1) / 2.0 - viewport_height_doc / 2.0;
            }
            GpuDestinationView::FitBoundingBoxWidth {
                top,
                content_bounds,
            } => {
                let Some(rect) = content_bounds.and_then(|bounds| {
                    Self::gpu_destination_rect(page.bounds, self.rotation, bounds)
                }) else {
                    return;
                };
                let Some((view_x, view_y)) =
                    Self::transformed_destination_axes(page.bounds, self.rotation, None, *top)
                else {
                    return;
                };
                if self.rotation.swaps_width_and_height() {
                    if let Some(zoom) =
                        self.destination_zoom_for_extent(self.viewport_height, rect.height())
                    {
                        self.set_zoom(zoom, cx);
                    }
                    target_x = view_x
                        .map(|left| self.destination_left_scroll(&page, left))
                        .unwrap_or(retained_x);
                    target_y = page.offset_y + rect.y0;
                } else {
                    if let Some(zoom) =
                        self.destination_zoom_for_extent(self.viewport_width, rect.width())
                    {
                        self.set_zoom(zoom, cx);
                    }
                    target_x = self.destination_center_scroll(&page, (rect.x0 + rect.x1) / 2.0);
                    target_y = view_y
                        .map(|top| page.offset_y + top)
                        .unwrap_or(page.offset_y + retained_top);
                }
            }
            GpuDestinationView::FitBoundingBoxHeight {
                left,
                content_bounds,
            } => {
                let Some(rect) = content_bounds.and_then(|bounds| {
                    Self::gpu_destination_rect(page.bounds, self.rotation, bounds)
                }) else {
                    return;
                };
                let Some((view_x, view_y)) =
                    Self::transformed_destination_axes(page.bounds, self.rotation, *left, None)
                else {
                    return;
                };
                if self.rotation.swaps_width_and_height() {
                    if let Some(zoom) =
                        self.destination_zoom_for_extent(self.viewport_width, rect.width())
                    {
                        self.set_zoom(zoom, cx);
                    }
                    target_x = self.destination_center_scroll(&page, (rect.x0 + rect.x1) / 2.0);
                    target_y = view_y
                        .map(|top| page.offset_y + top)
                        .unwrap_or(page.offset_y + retained_top);
                } else {
                    if let Some(zoom) =
                        self.destination_zoom_for_extent(self.viewport_height, rect.height())
                    {
                        self.set_zoom(zoom, cx);
                    }
                    target_x = view_x
                        .map(|left| self.destination_left_scroll(&page, left))
                        .unwrap_or(retained_x);
                    target_y = page.offset_y + rect.y0;
                }
            }
            GpuDestinationView::FitRectangle(rect) => {
                let rect = Rect::new(
                    rect[0] as f32,
                    rect[1] as f32,
                    rect[2] as f32,
                    rect[3] as f32,
                );
                let rect = Self::pdf_rect_to_view(page.bounds, self.rotation, rect);
                let zoom = self
                    .destination_zoom_for_extent(self.viewport_width, rect.width())
                    .zip(self.destination_zoom_for_extent(self.viewport_height, rect.height()))
                    .map(|(width, height)| width.min(height));
                if let Some(zoom) = zoom {
                    self.set_zoom(zoom, cx);
                }
                target_x = self.destination_center_scroll(&page, (rect.x0 + rect.x1) / 2.0);
                let viewport_height_doc = self.viewport_height / self.effective_scale();
                target_y = page.offset_y + (rect.y0 + rect.y1) / 2.0 - viewport_height_doc / 2.0;
            }
        }
        self.set_scroll_doc(target_x, target_y, cx);
    }

    pub(super) fn gpu_destination_rect(
        page_bounds: Rect,
        rotation: DocumentRotation,
        bounds: [f64; 4],
    ) -> Option<Rect> {
        let values = bounds.map(|value| value as f32);
        if values.iter().any(|value| !value.is_finite()) {
            return None;
        }
        let rect = Rect::new(values[0], values[1], values[2], values[3]);
        Some(Self::pdf_rect_to_view(page_bounds, rotation, rect))
    }

    pub(super) fn activate_gpu_link_action(
        &mut self,
        action: &GpuLinkAction,
        canonical_click: Option<[f64; 2]>,
        cx: &mut Context<Self>,
    ) {
        if action.requires_content_bounds() {
            self.start_native_gpu_action_resolution(action.clone(), canonical_click, cx);
            return;
        }
        match action {
            GpuLinkAction::GoTo { page, view } => {
                self.navigate_to_gpu_destination(*page as usize, view, cx);
            }
            GpuLinkAction::Uri(uri) => cx.open_url(uri),
            GpuLinkAction::UriMap(action) => {
                if let Some([x, y]) = canonical_click
                    && let Some(uri) = action.resolve_at(x, y)
                {
                    cx.open_url(&uri);
                }
            }
            GpuLinkAction::Named(action) => {
                let current = self.current_page_index();
                let page = match action {
                    GpuNamedAction::NextPage => {
                        current.saturating_add(1).min(self.last_page_index())
                    }
                    GpuNamedAction::PreviousPage => current.saturating_sub(1),
                    GpuNamedAction::FirstPage => 0,
                    GpuNamedAction::LastPage => self.last_page_index(),
                };
                self.navigate_to_pdf_destination(page, None, None, cx);
            }
            GpuLinkAction::Sequence(actions) => {
                for action in actions.iter() {
                    self.activate_gpu_link_action(action, canonical_click, cx);
                }
            }
            GpuLinkAction::NoOp { code, message } => {
                eprintln!("native PDF inert Link action [{code}]: {message}");
            }
        }
    }

    pub(super) fn navigate_to_pdf_destination(
        &mut self,
        page_idx: usize,
        target_top: Option<f32>,
        target_left: Option<f32>,
        cx: &mut Context<Self>,
    ) {
        if self.pages.get(page_idx).is_none() {
            return;
        }

        if self.is_presentation_mode() {
            let current_position = self.current_jump_position();
            Self::push_jump_position(&mut self.jump_back, current_position);
            self.jump_forward.clear();
            self.mark_manual_navigation();
            self.set_presentation_page(page_idx, cx);
            return;
        }

        let Some((scroll_x_doc, scroll_y_doc)) =
            self.pdf_destination_scroll_position(page_idx, target_top, target_left)
        else {
            return;
        };

        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        self.jump_forward.clear();
        self.mark_manual_navigation();
        self.set_scroll_doc(scroll_x_doc, scroll_y_doc, cx);
    }

    pub(super) fn pdf_destination_scroll_position(
        &self,
        page_idx: usize,
        target_top: Option<f32>,
        target_left: Option<f32>,
    ) -> Option<(f32, f32)> {
        let page = self.pages.get(page_idx)?;
        let target_point = Self::pdf_point_to_view(
            page.bounds,
            self.rotation,
            mupdf::Point {
                x: target_left.unwrap_or(page.bounds.x0 + page.width / 2.0),
                y: target_top.unwrap_or(page.bounds.y0),
            },
        );
        let swapped = self.rotation.swaps_width_and_height();
        let scroll_y_doc =
            if (swapped && target_left.is_some()) || (!swapped && target_top.is_some()) {
                page.offset_y + target_point.y
            } else {
                page.offset_y
            };
        let scroll_x_doc =
            if (swapped && target_top.is_some()) || (!swapped && target_left.is_some()) {
                page.offset_x + target_point.x - self.max_page_width / 2.0
            } else {
                self.scroll_x_doc
            };

        Some((
            if scroll_x_doc.is_finite() {
                scroll_x_doc
            } else {
                self.scroll_x_doc
            },
            if scroll_y_doc.is_finite() {
                scroll_y_doc
            } else {
                page.offset_y
            },
        ))
    }

    pub(crate) fn navigate_to_page_number(&mut self, page_number: usize, cx: &mut Context<Self>) {
        if !(1..=self.page_count()).contains(&page_number) {
            return;
        }

        self.navigate_to_pdf_destination(page_number - 1, None, None, cx);
    }

    pub fn navigate_to_document_position(
        &mut self,
        position: DocumentPosition,
        cx: &mut Context<Self>,
    ) {
        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        self.jump_forward.clear();
        self.mark_manual_navigation();
        if self.capabilities.reflow && position.is_reflow() {
            self.restore_position(position);
            self.schedule_position_save(cx);
            cx.notify();
            return;
        }
        self.set_zoom(position.zoom, cx);
        self.set_scroll_doc(position.scroll_x_doc, position.scroll_y_doc, cx);
    }

    pub(crate) fn open_library_document(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(DocumentViewEvent::OpenDocument { path, position });
    }

    pub(crate) fn navigate_to_toc_entry(&mut self, entry_ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.toc.get(entry_ix) else {
            return;
        };
        if !entry.navigable {
            return;
        }

        let native_action = entry.native_action.clone();
        let page_idx = entry.page_idx;
        let target_top = entry.target_top;
        let target_left = entry.target_left;
        self.toc_sidebar_selected = Some(entry_ix);
        cx.notify();
        if let Some(action) = native_action {
            self.activate_gpu_link_action(&action, None, cx);
        } else if let Some(page_idx) = page_idx {
            self.navigate_to_pdf_destination(page_idx, target_top, target_left, cx);
        }
    }

    pub(crate) fn navigate_to_note(&mut self, note_ix: usize, cx: &mut Context<Self>) {
        let Some((note_id, page_idx, target_top)) = self.notes.get(note_ix).and_then(|note| {
            note.rects
                .iter()
                .min_by(|left, right| {
                    left.page_idx
                        .cmp(&right.page_idx)
                        .then_with(|| left.y0.total_cmp(&right.y0))
                        .then_with(|| left.x0.total_cmp(&right.x0))
                })
                .map(|rect| (note.id, rect.page_idx, rect.y0))
        }) else {
            return;
        };

        self.notes_sidebar_selected = Some(note_id);
        cx.notify();
        self.navigate_to_pdf_destination(page_idx, Some(target_top), None, cx);
    }

    pub(super) fn activate_link(
        &mut self,
        link_ix: usize,
        screen_position: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if let Some(action) = self
            .native_link_interaction_for_link(link_ix)
            .map(|interaction| interaction.action.clone())
        {
            let canonical_click = self
                .links
                .get(link_ix)
                .and_then(|link| self.screen_to_pdf(link.page_idx, screen_position))
                .map(|point| [f64::from(point.x), f64::from(point.y)]);
            self.activate_gpu_link_action(&action, canonical_click, cx);
            return;
        }
        let Some(target) = self.links.get(link_ix).map(|link| link.target.clone()) else {
            return;
        };

        match target {
            LinkTarget::Internal {
                page_idx,
                target_top,
                target_left,
            } => self.navigate_to_pdf_destination(page_idx, target_top, target_left, cx),
            LinkTarget::External(url) => cx.open_url(&url),
        }
    }
}
