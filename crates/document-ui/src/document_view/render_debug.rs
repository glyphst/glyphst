use super::*;

fn page_render_debug_status(
    requested: Option<RenderKey>,
    displayed: Option<RenderKey>,
    exact_pending: bool,
    requested_failed: bool,
) -> PageRenderDebugStatus {
    match (requested, displayed) {
        (Some(requested), Some(displayed)) if requested == displayed => {
            PageRenderDebugStatus::Exact
        }
        (Some(_), Some(_)) => PageRenderDebugStatus::Fallback { exact_pending },
        (Some(_), None) if exact_pending => PageRenderDebugStatus::Pending,
        (Some(_), None) if requested_failed => PageRenderDebugStatus::Failed,
        _ => PageRenderDebugStatus::Unavailable,
    }
}

/// Returns a read-only process-wide cache snapshot without changing LRU order.
pub fn render_cache_debug_snapshot(cx: &gpui::App) -> RenderCacheDebugSnapshot {
    cx.try_global::<RenderCache>()
        .map(RenderCache::debug_snapshot)
        .unwrap_or_default()
}

impl DocumentView {
    /// Returns diagnostics for every page in the latest viewport paint.
    /// The page label continues to identify the page or spread at the center.
    ///
    /// Grid overview deliberately has no page section: its many thumbnails do
    /// not have one meaningful "current page", while the process-wide cache
    /// snapshot remains useful to the caller.
    pub fn render_debug_snapshot(&self, cx: &gpui::App) -> DocumentRenderDebugSnapshot {
        let page_range = self.current_page_range();
        let page_label = Self::page_counter_label(page_range.clone(), self.pages.len());
        let cache = cx.try_global::<RenderCache>();
        let interaction = DocumentInteractionDebugSnapshot {
            source_path: self.path.clone(),
            document_layout: format!("{:?}", self.document_layout),
            page_layout: format!("{:?}", self.page_layout),
            rotation: format!("{:?}", self.rotation),
            grid_overview_open: self.grid_overview_open,
            viewport_width: self.viewport_width,
            viewport_height: self.viewport_height,
            zoom: self.zoom,
            target_zoom: self.target_zoom,
            effective_scale: self.effective_scale(),
            scroll_x_doc: self.scroll_x_doc,
            scroll_y_doc: self.scroll_y_doc,
            target_scroll_x_doc: self.target_scroll_x_doc,
            target_scroll_y_doc: self.target_scroll_y_doc,
            max_scroll_y_doc: self.max_scroll_y_doc(),
            motion_active: self.dock_snapshot.layout_motion_active
                || (self.target_zoom - self.zoom).abs() > f32::EPSILON
                || (self.target_scroll_x_doc - self.scroll_x_doc).abs() > f64::EPSILON
                || (self.target_scroll_y_doc - self.scroll_y_doc).abs() > f64::EPSILON,
        };
        let pages = if self.grid_overview_open {
            Vec::new()
        } else {
            let mut page_indices = if self.viewport_render_debug.is_empty() {
                page_range.collect::<Vec<_>>()
            } else {
                self.viewport_render_debug.keys().copied().collect()
            };
            page_indices.sort_unstable();
            page_indices
                .into_iter()
                .filter(|page_idx| *page_idx < self.pages.len())
                .map(|page_idx| self.page_render_debug_snapshot(page_idx, cache))
                .collect()
        };

        DocumentRenderDebugSnapshot {
            interaction,
            page_label,
            pages,
            pending_render_count: self.pending_renders.len(),
            pending_render_limit: MAX_PENDING_PAGE_RENDERS,
        }
    }

    fn page_render_debug_snapshot(
        &self,
        page_idx: usize,
        cache: Option<&RenderCache>,
    ) -> PageRenderDebugSnapshot {
        let viewport = self
            .reload_task
            .is_none()
            .then(|| self.viewport_render_debug.get(&page_idx).copied())
            .flatten();
        let requested = viewport.map(|viewport| viewport.requested);
        let displayed = viewport.and_then(|viewport| viewport.displayed);
        let exact_pending = requested.is_some_and(|requested| {
            self.pending_renders
                .iter()
                .any(|pending| pending.key == requested)
        });
        let latest_attempt = self
            .reload_task
            .is_none()
            .then(|| self.page_render_debug.get(&page_idx).copied())
            .flatten();
        let requested_failed = requested.is_some_and(|requested| {
            latest_attempt.is_some_and(|attempt| attempt.key == requested && !attempt.succeeded)
        });
        let status =
            page_render_debug_status(requested, displayed, exact_pending, requested_failed);

        let source_revision = self
            .page_render_sources
            .get(&page_idx)
            .map(|source| source.revision)
            .unwrap_or_default();
        let latest_timing = latest_attempt
            .filter(|attempt| {
                attempt.key.doc_id == self.doc_id && attempt.key.source_revision == source_revision
            })
            .map(|attempt| PageRenderTimingDebugSnapshot {
                total: attempt.total,
                display_list: attempt.display_list,
                display_list_cached: attempt.display_list_cached,
                raster: attempt.raster,
                mupdf_draw: attempt.mupdf_draw,
                raster_bands: attempt.raster_bands,
                succeeded: attempt.succeeded,
            });
        let (cached_raster_bytes, cached_raster_variants) = cache.map_or((0, 0), |cache| {
            cache
                .renders
                .iter()
                .filter(|cached| {
                    cached.key.doc_id == self.doc_id && cached.key.page_idx == page_idx
                })
                .fold((0u64, 0usize), |(bytes, variants), cached| {
                    (bytes.saturating_add(cached.key.byte_len()), variants + 1)
                })
        });
        let page = &self.pages[page_idx];

        PageRenderDebugSnapshot {
            page_number: page_idx + 1,
            status,
            requested_raster: requested.map(|key| self.raster_debug_snapshot(key)),
            displayed_raster: displayed.map(|key| self.raster_debug_snapshot(key)),
            cached_raster_bytes,
            cached_raster_variants,
            source_revision,
            latest_timing,
            glyph_count: page.glyph_range.len(),
            line_count: self
                .page_line_ranges
                .get(page_idx)
                .map(Range::len)
                .unwrap_or_default(),
            link_count: self
                .links
                .iter()
                .filter(|link| link.page_idx == page_idx)
                .count(),
            text_index_state: match self.text_index_state {
                TextIndexState::Pending => TextIndexDebugState::Pending,
                TextIndexState::Indexing => TextIndexDebugState::Indexing,
                TextIndexState::Ready => TextIndexDebugState::Ready,
                TextIndexState::Failed => TextIndexDebugState::Failed,
            },
        }
    }

    fn raster_debug_snapshot(&self, key: RenderKey) -> RasterDebugSnapshot {
        let page_width = self
            .pages
            .get(key.page_idx)
            .map(|page| Self::page_dimensions_for(page, key.rotation).0)
            .unwrap_or(key.physical_width as f32)
            .max(1.0);
        RasterDebugSnapshot {
            physical_width: key.physical_width,
            physical_height: key.physical_height,
            bytes: key.byte_len(),
            scale: key.physical_width as f32 / page_width,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_key(width: u32) -> RenderKey {
        RenderKey {
            doc_id: 1,
            page_idx: 0,
            source_revision: 0,
            rotation: DocumentRotation::Degrees0,
            color_transform: None,
            physical_width: width,
            physical_height: width * 2,
        }
    }

    #[test]
    fn mismatched_raster_is_reported_as_a_fallback() {
        let requested = render_key(1_100);
        let displayed = render_key(1_000);
        assert_eq!(
            page_render_debug_status(Some(requested), Some(displayed), true, false),
            PageRenderDebugStatus::Fallback {
                exact_pending: true
            }
        );
        assert_eq!(
            page_render_debug_status(Some(requested), Some(requested), false, false),
            PageRenderDebugStatus::Exact
        );
    }
}
