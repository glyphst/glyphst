use super::*;

fn gpu_fallback_debug_snapshot(diagnostic: &GpuFallbackDiagnostic) -> GpuFallbackDebugSnapshot {
    GpuFallbackDebugSnapshot {
        feature: diagnostic.feature.to_string(),
        code: diagnostic.code.clone(),
        message: diagnostic.message.clone(),
        source: match diagnostic.source {
            GpuFallbackSource::Frontend => "frontend",
            GpuFallbackSource::Renderer => "renderer",
            GpuFallbackSource::Application => "application",
        },
    }
}

/// Returns a read-only process-wide cache snapshot without changing LRU order.
pub fn render_cache_debug_snapshot(cx: &gpui::App) -> RenderCacheDebugSnapshot {
    cx.try_global::<RenderCache>()
        .map(RenderCache::debug_snapshot)
        .unwrap_or_default()
}

impl DocumentView {
    /// Returns diagnostics for the page or spread at the viewport center.
    ///
    /// Grid overview deliberately has no page section: its many thumbnails do
    /// not have one meaningful "current page", while the process-wide cache
    /// snapshot remains useful to the caller.
    pub fn render_debug_snapshot(&self, cx: &gpui::App) -> DocumentRenderDebugSnapshot {
        let page_range = self.current_page_range();
        let page_label = Self::page_counter_label(page_range.clone(), self.pages.len());
        let cache = cx.try_global::<RenderCache>();
        let configured_engine = cx
            .global::<SettingsStore>()
            .settings
            .document_view
            .pdf_render_engine;
        let pages = if self.grid_overview_open {
            Vec::new()
        } else {
            page_range
                .filter(|page_idx| *page_idx < self.pages.len())
                .map(|page_idx| self.page_render_debug_snapshot(page_idx, cache, configured_engine))
                .collect()
        };

        DocumentRenderDebugSnapshot {
            page_label,
            pages,
            pending_render_count: self.pending_renders.len(),
            pending_render_limit: MAX_PENDING_PAGE_RENDERS,
            gpu_cache: self.native_gpu_cache_debug(),
            gpu_navigation_fallbacks: self
                .native_gpu_navigation_fallback_diagnostics()
                .unwrap_or_default()
                .iter()
                .map(gpu_fallback_debug_snapshot)
                .collect(),
        }
    }

    fn page_render_debug_snapshot(
        &self,
        page_idx: usize,
        cache: Option<&RenderCache>,
        configured_engine: PdfRenderEngine,
    ) -> PageRenderDebugSnapshot {
        let viewport = self
            .reload_task
            .is_none()
            .then(|| self.viewport_render_debug.get(&page_idx).copied())
            .flatten();
        let requested = viewport.map(|viewport| viewport.requested);
        let mupdf_displayed = viewport.and_then(|viewport| viewport.displayed);
        let native_gpu_requested =
            requested.is_some_and(|key| self.native_gpu_key_enabled(key, configured_engine));
        let native_gpu_render = self
            .native_gpu_render(page_idx)
            .filter(|(key, _)| requested.is_some_and(|requested| requested == *key));
        let displayed = native_gpu_render.map(|(key, _)| key).or(mupdf_displayed);
        let exact_pending = requested.is_some_and(|requested| {
            self.pending_renders
                .iter()
                .any(|pending| pending.key == requested)
        }) || (native_gpu_requested && self.native_gpu_pending(page_idx));
        let latest_attempt = self
            .reload_task
            .is_none()
            .then(|| self.page_render_debug.get(&page_idx).copied())
            .flatten();
        let status = match (requested, displayed) {
            (Some(requested), Some(displayed)) if requested == displayed => {
                PageRenderDebugStatus::Exact
            }
            (Some(_), Some(_)) => PageRenderDebugStatus::Fallback { exact_pending },
            (Some(_), None) if exact_pending => PageRenderDebugStatus::Pending,
            (Some(requested), None)
                if latest_attempt
                    .is_some_and(|attempt| attempt.key == requested && !attempt.succeeded) =>
            {
                PageRenderDebugStatus::Failed
            }
            _ => PageRenderDebugStatus::Unavailable,
        };

        let gpu = native_gpu_render.map(|(_, stats)| GpuPageRenderDebugSnapshot {
            cpu_time: stats.cpu_time,
            gpu_time: stats.gpu_time,
            host_to_device_bytes: stats.host_to_device_bytes,
            passes_executed: stats.passes_executed,
            command_buffers: stats.command_buffers,
            vector_batches: stats.vector_batches,
            procedural_stroke_batches: stats.hairline_batches,
            render_bundle_runs: stats.render_bundle_runs,
            reprojected_frame: stats.reprojected_frame,
            gradient_draws: stats.gradient_draws,
            image_draws: stats.image_draws,
            patch_lod_required_scale: format!("{:.3}", stats.patch_lod_required_scale),
            patch_lod_available_scale: format!("{:.3}", stats.patch_lod_available_scale),
            patch_lod_deferred: stats.patch_lod_deferred,
            reused_tiles: stats.reused_tiles,
            rendered_tiles: stats.rendered_tiles,
            dirty_tiles: stats.dirty_tiles,
            refinement_pending: stats.refinement_pending,
            selected_vector_lod: stats.selected_vector_lod,
            frame_pool_reused: stats.frame_pool_reused,
            clip_state_changes: stats.clip_state_changes,
            stencil_draws: stats.stencil_draws,
            compute_dispatches: stats.compute_dispatches,
            offscreen_passes: stats.offscreen_passes,
            mask_passes: stats.mask_passes,
            composite_passes: stats.composite_passes,
            backdrop_copies: stats.backdrop_copies,
            transient_peak_bytes: stats.transient_peak_bytes,
            transient_resident_bytes: stats.transient_resident_bytes,
            transient_pool_reuses: stats.transient_pool_reuses,
            generation: stats.generation,
            coverage_strategy: format!("{:?}", stats.coverage_strategy),
        });
        let patch_lod_preparation = self
            .native_gpu_patch_lod_preparation_stats(page_idx)
            .unwrap_or_default();
        let gpu_preparation = self.native_gpu_preparation_stats(page_idx).map(|stats| {
            GpuPagePreparationDebugSnapshot {
                elapsed: stats.elapsed,
                host_to_device_bytes: stats.host_to_device_bytes,
                resident_bytes: stats.resident_bytes,
                image_textures: stats.image_textures,
                alpha_textures: stats.alpha_textures,
                gradient_tables: stats.gradient_tables,
                patch_shading_lod_levels: stats.patch_shading_lod_levels,
                patch_shading_deferred_lod_levels: stats.patch_shading_deferred_lod_levels,
                patch_shading_mesh_bytes: stats.mesh_shading_bytes,
                deferred_patch_lod_elapsed: patch_lod_preparation.elapsed,
                deferred_patch_lod_levels: patch_lod_preparation.prepared_lod_levels,
                deferred_patch_lod_bytes: patch_lod_preparation.mesh_bytes,
                vector_batches: stats.vector_batches,
                vector_lod_levels: stats.vector_lod_levels,
                vector_lod_compacted: stats.vector_lod_compacted,
                vector_vertices: stats.vector_vertices,
                vector_triangle_references: stats.vector_triangle_references,
                vector_reference_bytes: stats.vector_reference_bytes,
                vector_compact_reference_lods: stats.vector_compact_reference_lods,
                vector_instances: stats.vector_instances,
                vector_bytes: stats.vector_bytes,
                procedural_stroke_batches: stats.hairline_batches,
                hairline_lod_levels: stats.hairline_lod_levels,
                hairline_geometry_records: stats.hairline_geometry_records,
                stroke_state_records: stats.stroke_state_records,
                hairline_primitives: stats.hairline_primitives,
                hairline_draws: stats.hairline_draws,
                hairline_bytes: stats.hairline_bytes,
                hairline_chunks: stats.hairline_chunks,
                procedural_stroke_lod_levels: stats.procedural_stroke_lod_levels,
                procedural_stroke_primitives: stats.procedural_stroke_primitives,
                procedural_stroke_draws: stats.procedural_stroke_draws,
                procedural_stroke_bytes: stats.procedural_stroke_bytes,
                render_bundle_runs: stats.render_bundle_runs,
                render_bundle_operations: stats.render_bundle_operations,
                retained_operations: stats.retained_operations,
                retained_form_resources: stats.retained_form_resources,
                retained_form_instances: stats.retained_form_instances,
                retained_type3_glyph_resources: stats.retained_type3_glyph_resources,
                retained_type3_glyph_instances: stats.retained_type3_glyph_instances,
                retained_tiling_pattern_resources: stats.retained_tiling_pattern_resources,
                retained_tiling_pattern_instances: stats.retained_tiling_pattern_instances,
                expanded_tiling_pattern_cells: stats.expanded_tiling_pattern_cells,
                retained_transparency_groups: stats.retained_transparency_groups,
                retained_soft_mask_resources: stats.retained_soft_mask_resources,
                retained_soft_mask_instances: stats.retained_soft_mask_instances,
                retained_subscene_nodes: stats.retained_subscene_nodes,
                expanded_scene_nodes: stats.expanded_scene_nodes,
                unique_clip_paths: stats.unique_clip_paths,
                analytic_paths: stats.analytic_paths,
                analytic_segments: stats.analytic_segments,
                analytic_draws: stats.analytic_draws,
                analytic_groups: stats.analytic_groups,
                analytic_bvh_nodes: stats.analytic_bvh_nodes,
                analytic_bytes: stats.analytic_bytes,
                analytic_fallback: stats.analytic_fallback.map(str::to_owned),
            }
        });
        let gpu_fallbacks = self
            .native_gpu_fallback_diagnostics(page_idx)
            .unwrap_or_default()
            .iter()
            .map(gpu_fallback_debug_snapshot)
            .collect::<Vec<_>>();
        let gpu_failure = self.native_gpu_failure(page_idx).map(str::to_owned);
        let backend = if gpu.is_some() {
            PageRenderBackendDebug::GlyphstGpu
        } else if !gpu_fallbacks.is_empty() || gpu_failure.is_some() {
            PageRenderBackendDebug::MuPdfFallback
        } else if native_gpu_requested {
            PageRenderBackendDebug::MuPdfWhileGpuPending
        } else {
            PageRenderBackendDebug::MuPdf
        };

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
            backend,
            gpu,
            gpu_preparation,
            gpu_fallbacks,
            gpu_failure,
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
