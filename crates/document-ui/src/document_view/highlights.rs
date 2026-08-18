use super::*;

impl DocumentView {
    pub(super) fn highlight_overlays(&self) -> (Vec<HighlightOverlay>, Vec<Bounds<Pixels>>) {
        let mut overlays = Vec::new();
        let mut selected_borders = Vec::new();

        for highlight in &self.highlights {
            for rect in &highlight.rects {
                let Some(bounds) = self.highlight_rect_to_screen(rect) else {
                    continue;
                };
                overlays.push((bounds, highlight.color));
                if self.selected_highlight == Some(highlight.id) {
                    selected_borders.push(bounds);
                }
            }
        }

        (overlays, selected_borders)
    }

    pub(super) fn note_underlines(&self) -> Vec<(Bounds<Pixels>, HighlightColor)> {
        let mut underlines = Vec::new();

        for note in &self.notes {
            for rect in &note.rects {
                let Some(bounds) = self.highlight_rect_to_screen(rect) else {
                    continue;
                };
                underlines.push((bounds, note.color));
            }
        }

        underlines
    }

    fn highlight_rect_to_screen(&self, rect: &HighlightRect) -> Option<Bounds<Pixels>> {
        let image_bounds = *self.page_image_bounds.get(rect.page_idx)?.as_ref()?;
        Some(self.pdf_rect_to_screen(
            rect.page_idx,
            Rect::new(rect.x0, rect.y0, rect.x1, rect.y1),
            image_bounds,
        ))
    }

    pub(super) fn selection_highlights(&self) -> Vec<Bounds<Pixels>> {
        let Some((start, end)) = self.selection_bounds() else {
            return Vec::new();
        };

        self.highlight_bounds_for_range(start..end + 1)
    }

    pub(super) fn point_in_text_selection(&self, point: GpuiPoint<Pixels>) -> bool {
        self.selection_highlights()
            .iter()
            .any(|bounds| bounds.contains(&point))
    }

    pub(super) fn link_highlights(&self) -> Vec<Bounds<Pixels>> {
        let Some(link_ix) = self.hovered_link else {
            return Vec::new();
        };
        let Some(link) = self.links.get(link_ix) else {
            return Vec::new();
        };

        let mut highlights = Vec::new();
        for rect in &link.rects {
            self.push_highlight(&mut highlights, link.page_idx, *rect);
        }

        highlights
    }

    pub(super) fn search_highlights(&self) -> (Vec<Bounds<Pixels>>, Vec<Bounds<Pixels>>) {
        let mut highlights = Vec::new();
        let mut active_highlights = Vec::new();

        for (match_ix, search_match) in self.search.matches.iter().enumerate() {
            let bounds = self.highlight_bounds_for_range(search_match.range.clone());
            if self.search.active == Some(match_ix) {
                active_highlights.extend(bounds);
            } else {
                highlights.extend(bounds);
            }
        }

        (highlights, active_highlights)
    }

    pub(super) fn thumbnail_overlays_for_page(&self, page_idx: usize) -> PageThumbnailOverlays {
        Self::thumbnail_overlays_for_page_data(
            page_idx,
            &self.highlights,
            &self.notes,
            &self.glyphs,
            &self.search.matches,
            self.search.active,
        )
    }

    pub(super) fn thumbnail_overlays_for_page_data(
        page_idx: usize,
        highlights: &[Highlight],
        notes: &[Note],
        glyphs: &[Glyph],
        matches: &[SearchMatch],
        active_match: Option<usize>,
    ) -> PageThumbnailOverlays {
        let highlights = highlights
            .iter()
            .flat_map(|highlight| {
                highlight
                    .rects
                    .iter()
                    .filter(move |rect| rect.page_idx == page_idx)
                    .map(move |rect| (*rect, highlight.color))
            })
            .collect();
        let note_underlines = notes
            .iter()
            .flat_map(|note| {
                note.rects
                    .iter()
                    .filter(move |rect| rect.page_idx == page_idx)
                    .map(move |rect| (*rect, note.color))
            })
            .collect();

        // Search matches are appended page-by-page by `recompute_search`, so
        // binary partitioning avoids scanning every document match once per
        // visible thumbnail.
        let first_match = matches.partition_point(|search_match| search_match.page_idx < page_idx);
        let end_match = first_match
            + matches[first_match..]
                .partition_point(|search_match| search_match.page_idx == page_idx);
        let mut search_highlights = Vec::new();
        for (match_ix, search_match) in matches[first_match..end_match]
            .iter()
            .enumerate()
            .map(|(offset, search_match)| (first_match + offset, search_match))
        {
            search_highlights.extend(
                Self::highlight_rects_for_glyph_range(glyphs, search_match.range.clone())
                    .into_iter()
                    .filter(|rect| rect.page_idx == page_idx)
                    .map(|rect| SearchHighlightRect {
                        rect,
                        active: active_match == Some(match_ix),
                    }),
            );
        }

        PageThumbnailOverlays {
            highlights,
            note_underlines,
            search_highlights,
        }
    }

    pub(super) fn highlight_rects_for_range(&self, range: Range<usize>) -> Vec<HighlightRect> {
        Self::highlight_rects_for_glyph_range(&self.glyphs, range)
    }

    fn highlight_rects_for_glyph_range(
        glyphs: &[Glyph],
        range: Range<usize>,
    ) -> Vec<HighlightRect> {
        if range.start >= range.end || range.end > glyphs.len() {
            return Vec::new();
        }

        let mut rects = Vec::new();
        let mut current_page = None;
        let mut current_line = None;
        let mut current_rect = None;

        for glyph in &glyphs[range] {
            let rect = Self::glyph_line_rect(glyph);

            match (current_page, current_line, current_rect) {
                (Some(page), Some(line), Some(existing))
                    if page == glyph.page_idx && line == glyph.line_idx =>
                {
                    current_rect = Some(Self::union_rect(existing, rect));
                }
                (Some(page), Some(_), Some(existing)) => {
                    Self::push_highlight_rect(&mut rects, page, existing);
                    current_page = Some(glyph.page_idx);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
                _ => {
                    current_page = Some(glyph.page_idx);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
            }
        }

        if let (Some(page), Some(rect)) = (current_page, current_rect) {
            Self::push_highlight_rect(&mut rects, page, rect);
        }

        rects
    }

    pub(super) fn highlight_bounds_for_range(&self, range: Range<usize>) -> Vec<Bounds<Pixels>> {
        if range.start >= range.end || range.end > self.glyphs.len() {
            return Vec::new();
        }

        let mut highlights = Vec::new();
        let mut current_page = None;
        let mut current_line = None;
        let mut current_rect = None;

        for glyph in &self.glyphs[range] {
            let rect = Self::glyph_line_rect(glyph);

            match (current_page, current_line, current_rect) {
                (Some(page), Some(line), Some(existing))
                    if page == glyph.page_idx && line == glyph.line_idx =>
                {
                    current_rect = Some(Self::union_rect(existing, rect));
                }
                (Some(page), Some(_), Some(existing)) => {
                    self.push_highlight(&mut highlights, page, existing);
                    current_page = Some(glyph.page_idx);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
                _ => {
                    current_page = Some(glyph.page_idx);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
            }
        }

        if let (Some(page), Some(rect)) = (current_page, current_rect) {
            self.push_highlight(&mut highlights, page, rect);
        }

        highlights
    }

    fn push_highlight(&self, highlights: &mut Vec<Bounds<Pixels>>, page_idx: usize, rect: Rect) {
        let Some(Some(image_bounds)) = self.page_image_bounds.get(page_idx) else {
            return;
        };

        highlights.push(self.pdf_rect_to_screen(page_idx, rect, *image_bounds));
    }

    fn push_highlight_rect(rects: &mut Vec<HighlightRect>, page_idx: usize, rect: Rect) {
        let Some(rect) = Self::normalized_rect(rect) else {
            return;
        };

        rects.push(HighlightRect {
            page_idx,
            x0: rect.x0,
            y0: rect.y0,
            x1: rect.x1,
            y1: rect.y1,
        });
    }

    pub(super) fn pdf_rect_to_screen(
        &self,
        page_idx: usize,
        rect: Rect,
        image_bounds: Bounds<Pixels>,
    ) -> Bounds<Pixels> {
        let page = &self.pages[page_idx];
        Self::pdf_rect_to_image_bounds(
            page.bounds,
            self.rotation,
            self.page_dimensions(page_idx),
            rect,
            image_bounds,
        )
    }

    pub(super) fn pdf_rect_to_image_bounds(
        page_bounds: Rect,
        rotation: DocumentRotation,
        page_dimensions: (f32, f32),
        rect: Rect,
        image_bounds: Bounds<Pixels>,
    ) -> Bounds<Pixels> {
        let (page_width, page_height) = page_dimensions;
        let scale_x = image_bounds.size.width.as_f32() / page_width;
        let scale_y = image_bounds.size.height.as_f32() / page_height;
        let rect = Self::pdf_rect_to_view(page_bounds, rotation, rect);
        let x0 = image_bounds.origin.x.as_f32() + rect.x0 * scale_x;
        let y0 = image_bounds.origin.y.as_f32() + rect.y0 * scale_y;
        let x1 = image_bounds.origin.x.as_f32() + rect.x1 * scale_x;
        let y1 = image_bounds.origin.y.as_f32() + rect.y1 * scale_y;

        Bounds::new(
            point(px(x0), px(y0)),
            size(px((x1 - x0).max(1.0)), px((y1 - y0).max(1.0))),
        )
    }

    pub(super) fn quad_rect(quad: &Quad) -> Rect {
        Rect::new(
            quad.ul.x.min(quad.ur.x).min(quad.ll.x).min(quad.lr.x),
            quad.ul.y.min(quad.ur.y).min(quad.ll.y).min(quad.lr.y),
            quad.ul.x.max(quad.ur.x).max(quad.ll.x).max(quad.lr.x),
            quad.ul.y.max(quad.ur.y).max(quad.ll.y).max(quad.lr.y),
        )
    }

    pub(super) fn glyph_line_rect(glyph: &Glyph) -> Rect {
        Self::quad_rect(&glyph.quad)
    }

    pub(super) fn dotted_underline_segments(
        bounds: Bounds<Pixels>,
        dot: Pixels,
    ) -> Vec<Bounds<Pixels>> {
        let dot = px(dot.as_f32().max(0.5));
        let gap = dot;
        let step = dot + gap;
        let inset = dot / 3.0;
        let y = bounds.bottom() - dot - inset;
        let right = bounds.right();
        let mut x = bounds.left();
        let mut segments = Vec::new();
        while x < right {
            let width = (right - x).min(dot);
            segments.push(Bounds::new(point(x, y), size(width, dot)));
            x += step;
        }
        segments
    }

    pub(super) fn union_rect(a: Rect, b: Rect) -> Rect {
        Rect::new(
            a.x0.min(b.x0),
            a.y0.min(b.y0),
            a.x1.max(b.x1),
            a.y1.max(b.y1),
        )
    }

    pub(super) fn normalized_rect(rect: Rect) -> Option<Rect> {
        let x0 = rect.x0.min(rect.x1);
        let y0 = rect.y0.min(rect.y1);
        let x1 = rect.x0.max(rect.x1);
        let y1 = rect.y0.max(rect.y1);

        (x1 - x0 > f32::EPSILON && y1 - y0 > f32::EPSILON).then(|| Rect::new(x0, y0, x1, y1))
    }

    pub(super) fn rects_intersect(a: Rect, b: Rect) -> bool {
        a.x0 < b.x1 && a.x1 > b.x0 && a.y0 < b.y1 && a.y1 > b.y0
    }

    pub(super) fn distance_to_rect(point: mupdf::Point, rect: Rect) -> f32 {
        Self::squared_distance_to_rect(point, rect).sqrt()
    }

    pub(super) fn squared_distance_to_rect(point: mupdf::Point, rect: Rect) -> f32 {
        let dx = if point.x < rect.x0 {
            rect.x0 - point.x
        } else if point.x > rect.x1 {
            point.x - rect.x1
        } else {
            0.0
        };
        let dy = if point.y < rect.y0 {
            rect.y0 - point.y
        } else if point.y > rect.y1 {
            point.y - rect.y1
        } else {
            0.0
        };

        dx * dx + dy * dy
    }

    pub(super) fn with_alpha(rgb: u32, alpha: u8) -> u32 {
        ((rgb & 0x00ff_ffff) << 8) | alpha as u32
    }
}
