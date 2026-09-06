use super::*;

impl DocumentView {
    pub(super) fn restore_position(&mut self, position: DocumentPosition) {
        if self.capabilities.reflow
            && let Some(reflow) = position.semantic.as_ref()
        {
            self.zoom = position.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
            self.target_zoom = self.zoom;
            self.zoom_anchor = None;
            self.last_animation_frame = None;
            let progress_scroll =
                f64::from(reflow.progress.clamp(0.0, 1.0)) * self.max_scroll_y_doc();
            if self.glyphs.is_empty() {
                self.scroll_x_doc = 0.0;
                self.scroll_y_doc = progress_scroll;
                self.target_scroll_x_doc = 0.0;
                self.target_scroll_y_doc = progress_scroll;
                self.clamp_scroll();
                self.pending_semantic_position = Some(position);
                return;
            }
            if !reflow.anchor.is_empty()
                && let Some(glyph_ix) = Self::resolve_semantic_anchor(&self.glyphs, &reflow.anchor)
                && let Some(scroll_y) = self.scroll_y_for_glyph(glyph_ix)
            {
                self.scroll_x_doc = 0.0;
                self.scroll_y_doc = scroll_y
                    - f64::from(
                        reflow.viewport_fraction * (self.viewport_height / self.effective_scale()),
                    );
                self.target_scroll_x_doc = 0.0;
                self.target_scroll_y_doc = self.scroll_y_doc;
                self.clamp_scroll();
                self.pending_semantic_position = None;
                return;
            }
            self.scroll_x_doc = 0.0;
            self.scroll_y_doc = progress_scroll;
            self.target_scroll_x_doc = 0.0;
            self.target_scroll_y_doc = progress_scroll;
            self.clamp_scroll();
            self.pending_semantic_position = None;
            return;
        }
        let previous_camera = (self.zoom, self.scroll_x_doc, self.scroll_y_doc);
        self.zoom = position.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.target_zoom = self.zoom;
        self.zoom_anchor = None;
        self.last_animation_frame = None;
        self.scroll_x_doc = f64::from(position.scroll_x_doc);
        self.scroll_y_doc = f64::from(position.scroll_y_doc);
        self.target_scroll_x_doc = f64::from(position.scroll_x_doc);
        self.target_scroll_y_doc = f64::from(position.scroll_y_doc);
        self.clamp_scroll();
        if previous_camera != (self.zoom, self.scroll_x_doc, self.scroll_y_doc) {
            self.record_native_gpu_camera_step(previous_camera.0 != self.zoom);
        }
    }

    pub fn current_document_position(&self) -> DocumentPosition {
        if self.capabilities.reflow {
            let page_idx = self.page_index_for_scroll_y(self.target_scroll_y_doc);
            let progress = if self.max_scroll_y_doc() > 0.0 {
                self.target_scroll_y_doc / self.max_scroll_y_doc()
            } else {
                0.0
            };
            if self.glyphs.is_empty() {
                return DocumentPosition::reflow(
                    SemanticTextAnchor::empty(),
                    0.0,
                    progress as f32,
                    self.zoom,
                    page_idx + 1,
                );
            }
            let y_in_page = self
                .pages
                .get(page_idx)
                .map(|page| (self.target_scroll_y_doc - f64::from(page.offset_y)).max(0.0))
                .unwrap_or(0.0);
            let glyph_ix = self
                .pages
                .get(page_idx)
                .and_then(|page| {
                    page.glyph_range
                        .clone()
                        .find(|glyph_ix| {
                            self.glyphs.get(*glyph_ix).is_some_and(|glyph| {
                                f64::from(Self::glyph_line_rect(glyph).y1) >= y_in_page
                            })
                        })
                        .or(Some(page.glyph_range.start))
                })
                .unwrap_or(0)
                .min(self.glyphs.len().saturating_sub(1));
            let anchor = Self::semantic_anchor_for_glyph(&self.glyphs, glyph_ix);
            let viewport_height_doc = f64::from(self.viewport_height / self.effective_scale());
            let viewport_fraction = if viewport_height_doc > 0.0 {
                self.scroll_y_for_glyph(glyph_ix)
                    .map(|anchor_y| {
                        ((anchor_y - self.target_scroll_y_doc) / viewport_height_doc)
                            .clamp(0.0, 1.0)
                    })
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            return DocumentPosition::reflow(
                anchor,
                viewport_fraction as f32,
                progress as f32,
                self.zoom,
                page_idx + 1,
            );
        }
        if let Some(page_idx) = self.presentation_page_index() {
            return DocumentPosition::fixed(
                0.0,
                self.page_scroll_y_doc(page_idx) as f32,
                self.zoom,
                page_idx + 1,
            );
        }

        DocumentPosition::fixed(
            self.target_scroll_x_doc as f32,
            self.target_scroll_y_doc as f32,
            self.zoom,
            self.page_index_for_scroll_y(self.target_scroll_y_doc) + 1,
        )
    }

    pub(super) fn semantic_anchor_for_glyph(
        glyphs: &[Glyph],
        glyph_ix: usize,
    ) -> SemanticTextAnchor {
        const QUOTE_LEN: usize = 32;
        const CONTEXT_LEN: usize = 24;

        let glyph_ix = glyph_ix.min(glyphs.len().saturating_sub(1));
        let quote_end = (glyph_ix + QUOTE_LEN).min(glyphs.len());
        let prefix_start = glyph_ix.saturating_sub(CONTEXT_LEN);
        let suffix_end = (quote_end + CONTEXT_LEN).min(glyphs.len());
        SemanticTextAnchor::new(
            glyph_ix,
            glyphs[glyph_ix..quote_end]
                .iter()
                .map(|glyph| glyph.c)
                .collect::<String>(),
            glyphs[prefix_start..glyph_ix]
                .iter()
                .map(|glyph| glyph.c)
                .collect::<String>(),
            glyphs[quote_end..suffix_end]
                .iter()
                .map(|glyph| glyph.c)
                .collect::<String>(),
        )
    }

    pub(super) fn resolve_semantic_anchor(
        glyphs: &[Glyph],
        anchor: &SemanticTextAnchor,
    ) -> Option<usize> {
        if glyphs.is_empty() {
            return None;
        }
        let quote = anchor.quote.chars().collect::<Vec<_>>();
        let candidate = usize::try_from(anchor.logical_offset).ok()?;
        if candidate < glyphs.len() && Self::glyph_chars_match(glyphs, candidate, &quote) {
            return Some(candidate);
        }
        if quote.is_empty() {
            return Some(candidate.min(glyphs.len() - 1));
        }

        let mut fallback = None;
        for start in 0..glyphs.len() {
            if !Self::glyph_chars_match(glyphs, start, &quote) {
                continue;
            }
            fallback.get_or_insert(start);
            let prefix_start = start.saturating_sub(anchor.prefix.chars().count());
            let prefix = glyphs[prefix_start..start]
                .iter()
                .map(|glyph| glyph.c)
                .collect::<String>();
            let suffix_start = (start + quote.len()).min(glyphs.len());
            let suffix_end = (suffix_start + anchor.suffix.chars().count()).min(glyphs.len());
            let suffix = glyphs[suffix_start..suffix_end]
                .iter()
                .map(|glyph| glyph.c)
                .collect::<String>();
            if prefix.ends_with(&anchor.prefix) && suffix.starts_with(&anchor.suffix) {
                return Some(start);
            }
        }
        fallback.or_else(|| Some(candidate.min(glyphs.len() - 1)))
    }

    fn glyph_chars_match(glyphs: &[Glyph], start: usize, expected: &[char]) -> bool {
        start + expected.len() <= glyphs.len()
            && glyphs[start..start + expected.len()]
                .iter()
                .map(|glyph| glyph.c)
                .eq(expected.iter().copied())
    }

    fn scroll_y_for_glyph(&self, glyph_ix: usize) -> Option<f64> {
        let glyph = self.glyphs.get(glyph_ix)?;
        let page = self.pages.get(glyph.page_idx)?;
        Some(f64::from(page.offset_y + Self::glyph_line_rect(glyph).y0))
    }

    pub(super) fn semantic_range_for_selection(
        &self,
        start: usize,
        end: usize,
    ) -> SemanticTextRange {
        SemanticTextRange {
            start: Self::semantic_anchor_for_glyph(&self.glyphs, start),
            end: Self::semantic_anchor_for_glyph(&self.glyphs, end),
        }
    }
}
