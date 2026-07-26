use super::*;

impl PdfView {
    pub(super) fn screen_to_document(&self, point: GpuiPoint<Pixels>) -> Option<mupdf::Point> {
        let (page_idx, bounds) = self
            .page_image_bounds
            .iter()
            .enumerate()
            .find_map(|(page_idx, bounds)| bounds.as_ref().map(|bounds| (page_idx, bounds)))?;
        let page = self.pages.get(page_idx)?;
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let scale_x = bounds.size.width.as_f32() / page_width;
        let scale_y = bounds.size.height.as_f32() / page_height;
        if scale_x <= 0.0 || scale_y <= 0.0 {
            return None;
        }

        let page_left = (self.max_page_width - page_width) / 2.0;
        Some(mupdf::Point {
            x: (page_left + (point.x.as_f32() - bounds.origin.x.as_f32()) / scale_x)
                .clamp(0.0, self.max_page_width),
            y: (page.offset_y + (point.y.as_f32() - bounds.origin.y.as_f32()) / scale_y)
                .clamp(0.0, self.content_height_doc),
        })
    }

    pub(super) fn region_reference_for_drag(
        &self,
        drag: RegionDragState,
    ) -> Option<RegionImageReference> {
        Self::region_reference_for_pages(
            &self.pages,
            self.max_page_width,
            self.effective_scale(),
            self.rotation,
            drag,
        )
    }

    pub(super) fn region_reference_for_pages(
        pages: &[PdfPage],
        max_page_width: f32,
        scale: f32,
        rotation: DocumentRotation,
        drag: RegionDragState,
    ) -> Option<RegionImageReference> {
        let x0 = drag.anchor.x.min(drag.current.x);
        let y0 = drag.anchor.y.min(drag.current.y);
        let x1 = drag.anchor.x.max(drag.current.x);
        let y1 = drag.anchor.y.max(drag.current.y);
        if (x1 - x0) * scale < MIN_REGION_SELECTION_PX
            || (y1 - y0) * scale < MIN_REGION_SELECTION_PX
        {
            return None;
        }

        let display_height = |page: &PdfPage| Self::page_dimensions_for(page, rotation).1;
        let start_page = pages.partition_point(|page| page.offset_y + display_height(page) <= y0);
        let end_page = start_page + pages[start_page..].partition_point(|page| page.offset_y < y1);
        let regions = pages[start_page..end_page]
            .iter()
            .enumerate()
            .filter_map(|(relative_page_idx, page)| {
                let (page_width, page_height) = Self::page_dimensions_for(page, rotation);
                let page_left = (max_page_width - page_width) / 2.0;
                let page_right = page_left + page_width;
                let page_bottom = page.offset_y + page_height;
                let region_x0 = x0.max(page_left);
                let region_y0 = y0.max(page.offset_y);
                let region_x1 = x1.min(page_right);
                let region_y1 = y1.min(page_bottom);
                if region_x1 <= region_x0 || region_y1 <= region_y0 {
                    return None;
                }

                let source_rect = Self::view_rect_to_pdf(
                    page.bounds,
                    rotation,
                    Rect::new(
                        region_x0 - page_left,
                        region_y0 - page.offset_y,
                        region_x1 - page_left,
                        region_y1 - page.offset_y,
                    ),
                );
                Some(PageRegion {
                    page: PageIndex(start_page + relative_page_idx),
                    x0: source_rect.x0,
                    y0: source_rect.y0,
                    x1: source_rect.x1,
                    y1: source_rect.y1,
                })
            })
            .collect::<Vec<_>>();
        let reference = RegionImageReference { regions };
        (!reference.is_empty()).then_some(reference)
    }

    pub(super) fn active_region_reference(&self) -> Option<RegionImageReference> {
        self.region_drag
            .and_then(|drag| self.region_reference_for_drag(drag))
            .or_else(|| self.selected_region.clone())
    }

    pub(super) fn region_overlay_bounds(
        &self,
        reference: &RegionImageReference,
    ) -> Vec<Bounds<Pixels>> {
        reference
            .regions
            .iter()
            .filter_map(|region| {
                let image_bounds = *self.page_image_bounds.get(region.page.0)?.as_ref()?;
                Some(self.pdf_rect_to_screen(
                    region.page.0,
                    Rect::new(region.x0, region.y0, region.x1, region.y1),
                    image_bounds,
                ))
            })
            .collect()
    }

    pub(super) fn point_in_selected_region(&self, point: GpuiPoint<Pixels>) -> bool {
        self.selected_region.as_ref().is_some_and(|reference| {
            self.region_overlay_bounds(reference)
                .iter()
                .any(|bounds| bounds.contains(&point))
        })
    }

    pub(super) fn region_popover_position(&self) -> Option<GpuiPoint<Pixels>> {
        self.selected_region
            .as_ref()
            .and_then(|reference| self.region_overlay_bounds(reference).into_iter().last())
            .map(|bounds| point(bounds.right(), bounds.bottom() + px(6.0)))
    }

    pub(super) fn point_in_image(&self, point: GpuiPoint<Pixels>) -> bool {
        self.page_at_screen(point).is_some()
    }

    pub(super) fn page_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        self.page_image_bounds
            .iter()
            .enumerate()
            .find_map(|(page_idx, bounds)| {
                let bounds = bounds.as_ref()?;
                let x = point.x.as_f32();
                let y = point.y.as_f32();
                let x0 = bounds.origin.x.as_f32();
                let y0 = bounds.origin.y.as_f32();
                let x1 = x0 + bounds.size.width.as_f32();
                let y1 = y0 + bounds.size.height.as_f32();

                (x >= x0 && x <= x1 && y >= y0 && y <= y1).then_some(page_idx)
            })
    }

    pub(super) fn screen_to_pdf(
        &self,
        page_idx: usize,
        point: GpuiPoint<Pixels>,
    ) -> Option<mupdf::Point> {
        let bounds = self.page_image_bounds.get(page_idx)?.as_ref()?;
        let page = &self.pages[page_idx];
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let scale_x = bounds.size.width.as_f32() / page_width;
        let scale_y = bounds.size.height.as_f32() / page_height;
        if scale_x <= 0.0 || scale_y <= 0.0 {
            return None;
        }

        Some(Self::view_point_to_pdf(
            page.bounds,
            self.rotation,
            mupdf::Point {
                x: (point.x.as_f32() - bounds.origin.x.as_f32()) / scale_x,
                y: (point.y.as_f32() - bounds.origin.y.as_f32()) / scale_y,
            },
        ))
    }
}
