use super::*;

impl DocumentView {
    fn image_bounds_for_page(
        &self,
        page_idx: usize,
        bounds: Bounds<Pixels>,
        scale: f32,
    ) -> Bounds<Pixels> {
        let page = &self.pages[page_idx];
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let image_width = px(page_width * scale);
        let image_height = px(page_height * scale);
        let origin = point(
            bounds.origin.x
                + (bounds.size.width - px(self.max_page_width * scale)) / 2.0
                + px((page.offset_x - self.scroll_x_doc) * scale),
            bounds.origin.y + px((page.offset_y - self.scroll_y_doc) * scale),
        );

        Bounds::new(origin, size(image_width, image_height))
    }

    pub(super) fn image_bounds_and_render_key(
        &self,
        page_idx: usize,
        bounds: Bounds<Pixels>,
        scale: f32,
        scale_factor: f32,
    ) -> (Bounds<Pixels>, RenderKey) {
        let image_bounds = self.image_bounds_for_page(page_idx, bounds, scale);
        let image_bounds = Self::pixel_aligned_image_bounds(image_bounds, scale_factor);
        let key = self.render_key(page_idx, image_bounds, scale_factor);
        (image_bounds, key)
    }

    pub(super) fn render_key(
        &self,
        page_idx: usize,
        image_bounds: Bounds<Pixels>,
        scale_factor: f32,
    ) -> RenderKey {
        let scale_factor = scale_factor.max(0.001);
        let requested_width = (image_bounds.size.width.as_f32() * scale_factor)
            .ceil()
            .max(1.0);
        let requested_height = (image_bounds.size.height.as_f32() * scale_factor)
            .ceil()
            .max(1.0);
        let max_dimension = MAX_RENDER_TEXTURE_DIMENSION as f32;
        let texture_scale = (max_dimension / requested_width)
            .min(max_dimension / requested_height)
            .min(1.0);

        RenderKey {
            doc_id: self.doc_id,
            page_idx,
            source_revision: self
                .page_render_sources
                .get(&page_idx)
                .map(|source| source.revision)
                .unwrap_or_default(),
            rotation: self.rotation,
            physical_width: (requested_width * texture_scale)
                .round()
                .clamp(1.0, max_dimension) as u32,
            physical_height: (requested_height * texture_scale)
                .round()
                .clamp(1.0, max_dimension) as u32,
        }
    }

    pub(super) fn pixel_aligned_image_bounds(
        image_bounds: Bounds<Pixels>,
        scale_factor: f32,
    ) -> Bounds<Pixels> {
        let scale_factor = scale_factor.max(0.001);
        let physical_width = (image_bounds.size.width.as_f32() * scale_factor)
            .ceil()
            .max(1.0);
        let physical_height = (image_bounds.size.height.as_f32() * scale_factor)
            .ceil()
            .max(1.0);

        Bounds::new(
            image_bounds.origin,
            size(
                px(physical_width / scale_factor),
                px(physical_height / scale_factor),
            ),
        )
    }

    pub(super) fn effective_scale(&self) -> f32 {
        (self.base_scale * self.zoom).max(0.001)
    }

    pub(super) fn rotated_dimensions(
        width: f32,
        height: f32,
        rotation: DocumentRotation,
    ) -> (f32, f32) {
        if rotation.swaps_width_and_height() {
            (height, width)
        } else {
            (width, height)
        }
    }

    pub(super) fn page_dimensions_for(
        page: &DocumentPage,
        rotation: DocumentRotation,
    ) -> (f32, f32) {
        Self::rotated_dimensions(page.width, page.height, rotation)
    }

    pub(super) fn page_dimensions(&self, page_idx: usize) -> (f32, f32) {
        Self::page_dimensions_for(&self.pages[page_idx], self.rotation)
    }

    fn page_to_view_matrix(page_bounds: Rect, rotation: DocumentRotation) -> Matrix {
        Self::render_matrix(
            page_bounds,
            rotation,
            1.0,
            1.0,
            mupdf::Point { x: 0.0, y: 0.0 },
        )
    }

    pub(super) fn pdf_point_to_view(
        page_bounds: Rect,
        rotation: DocumentRotation,
        point: mupdf::Point,
    ) -> mupdf::Point {
        point.transform(&Self::page_to_view_matrix(page_bounds, rotation))
    }

    pub(super) fn view_point_to_pdf(
        page_bounds: Rect,
        rotation: DocumentRotation,
        point: mupdf::Point,
    ) -> mupdf::Point {
        let inverse = Self::page_to_view_matrix(page_bounds, rotation)
            .invert()
            .expect("document rotation matrix is invertible");
        point.transform(&inverse)
    }

    pub(super) fn pdf_rect_to_view(
        page_bounds: Rect,
        rotation: DocumentRotation,
        rect: Rect,
    ) -> Rect {
        rect.transform(&Self::page_to_view_matrix(page_bounds, rotation))
    }

    pub(super) fn view_rect_to_pdf(
        page_bounds: Rect,
        rotation: DocumentRotation,
        rect: Rect,
    ) -> Rect {
        let inverse = Self::page_to_view_matrix(page_bounds, rotation)
            .invert()
            .expect("document rotation matrix is invertible");
        rect.transform(&inverse)
    }

    pub(super) fn render_matrix(
        page_bounds: Rect,
        rotation: DocumentRotation,
        scale_x: f32,
        scale_y: f32,
        view_origin: mupdf::Point,
    ) -> Matrix {
        let width = page_bounds.x1 - page_bounds.x0;
        let height = page_bounds.y1 - page_bounds.y0;
        let (a, b, c, d, tx, ty) = match rotation.quarter_turns() {
            0 => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            1 => (0.0, 1.0, -1.0, 0.0, height, 0.0),
            2 => (-1.0, 0.0, 0.0, -1.0, width, height),
            3 => (0.0, -1.0, 1.0, 0.0, 0.0, width),
            _ => unreachable!("document rotation is normalized"),
        };
        Matrix::new(
            a * scale_x,
            b * scale_y,
            c * scale_x,
            d * scale_y,
            (tx - a * page_bounds.x0 - c * page_bounds.y0 - view_origin.x) * scale_x,
            (ty - b * page_bounds.x0 - d * page_bounds.y0 - view_origin.y) * scale_y,
        )
    }
}
