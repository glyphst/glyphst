//! Bounded cache snapshots, render diagnostics, and latency samples.

use super::*;

pub(super) fn push_surface_latency(samples: &mut VecDeque<Duration>, sample: Duration) {
    if samples.len() == MAX_SURFACE_LATENCY_SAMPLES {
        samples.pop_front();
    }
    samples.push_back(sample);
}

pub(super) fn surface_latency_p95(samples: &VecDeque<Duration>) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    let mut ordered = samples.iter().copied().collect::<Vec<_>>();
    ordered.sort_unstable();
    let index = ordered
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1);
    ordered[index.min(ordered.len() - 1)]
}

impl DocumentView {
    pub(in super::super) fn native_gpu_fallback_diagnostics(
        &self,
        page: usize,
    ) -> Option<&[GpuFallbackDiagnostic]> {
        let page = u32::try_from(page).ok()?;
        if let Some(diagnostics) = self.gpu_pdf.application_fallbacks.get(&page) {
            return Some(diagnostics);
        }
        match self.gpu_pdf.pages.get(&page)? {
            GpuPageState::Fallback(diagnostics) => Some(diagnostics),
            _ => None,
        }
    }

    pub(in super::super) fn native_gpu_warning_diagnostics(
        &self,
        page: usize,
    ) -> Option<&[GpuWarningDiagnostic]> {
        let page = u32::try_from(page).ok()?;
        match self.gpu_pdf.pages.get(&page)? {
            GpuPageState::Ready(page) => Some(page.warnings()),
            GpuPageState::Preparing { .. }
            | GpuPageState::Fallback(_)
            | GpuPageState::Failed(_) => None,
        }
    }

    pub(in super::super) fn native_gpu_navigation_fallback_diagnostics(
        &self,
    ) -> Option<&[GpuFallbackDiagnostic]> {
        self.gpu_pdf
            .destination_fallback
            .as_deref()
            .or(self.gpu_pdf.navigation_fallback.as_deref())
    }

    pub(in super::super) fn native_gpu_render(
        &self,
        page: usize,
    ) -> Option<(RenderKey, GpuRenderStats)> {
        let page = u32::try_from(page).ok()?;
        if !self.gpu_pdf.presented_pages.contains(&page) {
            return None;
        }
        if self.gpu_pdf.presented_continuity_pages.contains(&page) {
            return self
                .gpu_pdf
                .continuity_surfaces
                .get(&page)
                .map(|continuity| (continuity.key, continuity.stats));
        }
        self.gpu_pdf
            .rendered
            .get(&page)
            .map(|rendered| (rendered.key, rendered.latest_stats))
    }

    pub(in super::super) fn native_gpu_preparation_stats(
        &self,
        page: usize,
    ) -> Option<GpuPreparationStats> {
        let page = u32::try_from(page).ok()?;
        match self.gpu_pdf.pages.get(&page)? {
            GpuPageState::Ready(page) => Some(page.preparation_stats()),
            _ => None,
        }
    }

    pub(in super::super) fn native_gpu_preparation_timings(
        &self,
        page: usize,
    ) -> Option<GpuPagePreparationTimings> {
        let page = u32::try_from(page).ok()?;
        match self.gpu_pdf.pages.get(&page)? {
            GpuPageState::Ready(page) => Some(page.preparation_timings()),
            _ => None,
        }
    }

    pub(in super::super) fn native_gpu_patch_lod_preparation_stats(
        &self,
        page: usize,
    ) -> Option<GpuPatchLodPreparationStats> {
        let page = u32::try_from(page).ok()?;
        match self.gpu_pdf.pages.get(&page)? {
            GpuPageState::Ready(page) => Some(page.patch_lod_preparation_stats()),
            _ => None,
        }
    }

    pub(in super::super) fn native_gpu_cache_debug(&self) -> GpuCacheDebugSnapshot {
        let frontend = self
            .gpu_pdf
            .document
            .as_ref()
            .map(|document| document.frontend_cache_stats())
            .unwrap_or_default();
        let prepared = self
            .gpu_pdf
            .pages
            .values()
            .filter_map(|state| match state {
                GpuPageState::Ready(page) => {
                    Some((page.preparation_stats(), page.patch_lod_preparation_stats()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let direct_render = self
            .gpu_pdf
            .renderer
            .as_ref()
            .map(|renderer| renderer.direct_render_stats())
            .unwrap_or_default();
        GpuCacheDebugSnapshot {
            frontend_outline_fonts: frontend.outline_fonts,
            frontend_outline_font_bytes: frontend.outline_font_bytes,
            frontend_outline_font_hits: frontend.outline_font_hits,
            frontend_outline_font_misses: frontend.outline_font_misses,
            frontend_outline_font_evictions: frontend.outline_font_evictions,
            frontend_objects: frontend.interpreter_objects,
            frontend_object_hits: frontend.interpreter_object_hits,
            frontend_object_misses: frontend.interpreter_object_misses,
            frontend_object_evictions: frontend.interpreter_object_evictions,
            frontend_preflight_reports: frontend.preflight_reports,
            frontend_preflight_scans: frontend.completed_preflight_scans,
            compiled_pages: self
                .gpu_pdf
                .document
                .as_ref()
                .map_or(0, |document| document.compiled_cache_entries()),
            compiled_page_limit: GpuPdfDocument::compiled_cache_limit(),
            compiled_bytes: self
                .gpu_pdf
                .document
                .as_ref()
                .map_or(0, |document| document.compiled_cache_bytes()),
            compiled_byte_limit: self
                .gpu_pdf
                .document
                .as_ref()
                .map_or(0, |document| document.compiled_cache_byte_limit()),
            content_bound_pages: self
                .gpu_pdf
                .document
                .as_ref()
                .map_or(0, |document| document.content_bounds_cache_entries()),
            content_bound_page_limit: GpuPdfDocument::content_bounds_cache_limit(),
            prepared_pages: prepared.len(),
            prepared_page_limit: MAX_RETAINED_GPU_PAGES,
            resident_bytes: prepared
                .iter()
                .map(|(cold, deferred)| cold.resident_bytes.saturating_add(deferred.mesh_bytes))
                .sum(),
            resident_byte_limit: MAX_RETAINED_GPU_RESOURCE_BYTES,
            surface_count: self
                .gpu_pdf
                .rendered
                .values()
                .map(|rendered| rendered.presentation.surface_count())
                .sum::<usize>()
                .saturating_add(self.gpu_pdf.continuity_surfaces.len()),
            surface_bytes: self.gpu_pdf.surface_bytes(),
            surface_byte_limit: MAX_RETAINED_GPU_SURFACE_BYTES,
            continuity_surface_count: self.gpu_pdf.continuity_surfaces.len(),
            continuity_surface_bytes: self
                .gpu_pdf
                .continuity_surfaces
                .values()
                .map(|continuity| continuity.texture.byte_len())
                .sum(),
            surface_seed_tasks: self.gpu_pdf.surface_seed_tasks.len(),
            surface_in_flight_bytes: self
                .gpu_pdf
                .surface_seed_tasks
                .values()
                .map(|task| task.in_flight_bytes)
                .fold(0_u64, u64::saturating_add),
            surface_render_pending: self.gpu_pdf.surface_render_pending.len(),
            surface_pending_peak: self.gpu_pdf.surface_pending_peak,
            surface_renders_started: self.gpu_pdf.surface_render_started,
            surface_renders_completed: self.gpu_pdf.surface_render_completed,
            surface_renders_coalesced: self.gpu_pdf.surface_render_coalesced,
            surface_renders_superseded: self.gpu_pdf.surface_render_superseded,
            surface_renders_deferred: self.gpu_pdf.surface_render_deferred,
            surface_renders_discarded: self.gpu_pdf.surface_render_discarded,
            surface_render_cache_pressure: self.gpu_pdf.surface_render_cache_pressure,
            surface_scroll_reuses: self.gpu_pdf.surface_scroll_reuses,
            surface_scroll_prefetches: self.gpu_pdf.surface_scroll_prefetches,
            surface_scroll_misses: self.gpu_pdf.surface_scroll_misses,
            surface_sharp_pan_cache_presentations: self
                .gpu_pdf
                .surface_sharp_pan_cache_presentations,
            surface_zoom_reuses: self.gpu_pdf.surface_zoom_reuses,
            surface_zoom_quality_refreshes: self.gpu_pdf.surface_zoom_quality_refreshes,
            surface_zoom_coverage_refreshes: self.gpu_pdf.surface_zoom_coverage_refreshes,
            surface_zoom_settled_upgrades: self.gpu_pdf.surface_zoom_settled_upgrades,
            surface_zoom_motion_reprojections: self.gpu_pdf.surface_zoom_motion_reprojections,
            surface_zoom_motion_refreshes_suppressed: self
                .gpu_pdf
                .surface_zoom_motion_refreshes_suppressed,
            surface_zoom_motion_cancellations: self.gpu_pdf.surface_zoom_motion_cancellations,
            surface_zoom_emergency_renders: self.gpu_pdf.surface_zoom_emergency_renders,
            surface_zoom_live_redraws: direct_render.encoded,
            surface_zoom_cached_sharp_presentations: self
                .gpu_pdf
                .surface_zoom_cached_sharp_presentations,
            surface_zoom_live_failures: self.gpu_pdf.surface_zoom_live_failures,
            surface_zoom_gpu_holds: self.gpu_pdf.surface_zoom_gpu_holds,
            surface_zoom_animation_steps: self.gpu_pdf.surface_zoom_animation_steps,
            surface_zoom_admission_holds: self.gpu_pdf.surface_zoom_admission_holds,
            surface_zoom_worker_holds: self.gpu_pdf.surface_zoom_worker_holds,
            surface_zoom_sample_holds: self.gpu_pdf.surface_zoom_sample_holds,
            surface_zoom_presentation_holds: self.gpu_pdf.surface_zoom_presentation_holds,
            surface_zoom_scaled_presentations: self.gpu_pdf.surface_zoom_scaled_presentations,
            surface_zoom_target_reuses: self.gpu_pdf.surface_zoom_target_reuses,
            surface_zoom_target_allocations: self.gpu_pdf.surface_zoom_target_allocations,
            motion_render_jobs_started: 0,
            motion_render_jobs_completed: 0,
            motion_render_jobs_superseded: 0,
            motion_render_jobs_pending: 0,
            motion_render_jobs_peak: 0,
            motion_render_detached_target_reuses: 0,
            motion_render_inline_frames: direct_render.encoded,
            motion_render_inline_failures: self.gpu_pdf.motion_render_inline_failures,
            motion_render_callback_scheduled: direct_render.scheduled,
            motion_render_callback_executed: direct_render.executed,
            motion_render_callback_encoded: direct_render.encoded,
            motion_render_callback_superseded: direct_render.superseded,
            motion_render_callback_failures: direct_render.failed,
            motion_render_callback_scheduled_camera_serial: direct_render
                .last_scheduled_camera_serial,
            motion_render_callback_executed_camera_serial: direct_render
                .last_executed_camera_serial,
            motion_render_callback_encoded_camera_serial: direct_render.last_encoded_camera_serial,
            motion_render_callback_cpu_time: direct_render.last_cpu_time,
            motion_render_callback_frame_pool_wait_time: direct_render.last_frame_pool_wait_time,
            motion_render_callback_command_encoding_time: direct_render.last_command_encoding_time,
            motion_render_callback_frame_pool_grew: direct_render.last_frame_pool_grew,
            motion_render_callback_frame_pool_capacity_bytes: direct_render
                .last_frame_pool_capacity_bytes,
            motion_render_callback_target_size: direct_render.last_target_size,
            motion_render_callback_passes_executed: direct_render.last_passes_executed,
            motion_render_callback_coverage_strategy: direct_render.last_coverage_strategy,
            motion_render_callback_presentation_quality: direct_render.last_presentation_quality,
            motion_render_callback_transient_peak_bytes: direct_render.last_transient_peak_bytes,
            motion_render_callback_transient_resident_bytes: direct_render
                .last_transient_resident_bytes,
            motion_render_callback_render_bundle_runs: direct_render.last_render_bundle_runs,
            motion_render_callback_stencil_draws: direct_render.last_stencil_draws,
            motion_render_callback_retained_stroke_candidate_references: direct_render
                .last_retained_stroke_candidate_references,
            motion_render_callback_retained_stroke_cull_dispatches: direct_render
                .last_retained_stroke_cull_dispatches,
            motion_render_callback_retained_stroke_indirect_draws: direct_render
                .last_retained_stroke_indirect_draws,
            motion_render_callback_retained_stroke_indirect_runs: direct_render
                .last_retained_stroke_indirect_runs,
            motion_render_callback_selected_stroke_lod: direct_render.last_selected_stroke_lod,
            motion_render_callback_active_stroke_lod_levels: direct_render
                .last_active_stroke_lod_levels,
            motion_render_callback_selected_stroke_segments: direct_render
                .last_selected_stroke_segments,
            motion_render_callback_target_visible_stroke_segments: direct_render
                .last_target_visible_stroke_segments,
            motion_render_callback_visible_stroke_tiles: direct_render.last_visible_stroke_tiles,
            motion_render_callback_max_visible_stroke_tile_segments: direct_render
                .last_max_visible_stroke_tile_segments,
            camera_pan_inline_frames: self.gpu_pdf.camera_pan_inline_frames,
            camera_pan_admission_holds: self.gpu_pdf.camera_pan_admission_holds,
            motion_render_camera_serial: self.gpu_pdf.motion_camera_serial,
            motion_render_presented_camera_serial: self
                .gpu_pdf
                .motion_presented_camera_serial
                .max(direct_render.last_encoded_camera_serial),
            motion_render_camera_serial_mismatches: self.gpu_pdf.motion_camera_serial_mismatches,
            motion_render_camera_batch_commits: self.gpu_pdf.motion_camera_batch_commits,
            camera_target_reservations_started: 0,
            camera_target_reservations_consumed: 0,
            camera_target_reservations_released: 0,
            camera_target_reservations_pending: 0,
            motion_render_queue_latency_p95: Duration::ZERO,
            motion_render_queue_latency_max: Duration::ZERO,
            motion_render_execution_latency_p95: Duration::ZERO,
            motion_render_execution_latency_max: Duration::ZERO,
            motion_render_total_latency_p95: Duration::ZERO,
            motion_render_total_latency_max: Duration::ZERO,
            surface_warm_requests: self.gpu_pdf.surface_warm_requests,
            surface_warm_hits: self.gpu_pdf.surface_warm_hits,
            surface_warm_pages: self
                .gpu_pdf
                .rendered
                .values()
                .filter(|rendered| rendered.prewarmed)
                .count()
                .saturating_add(
                    self.gpu_pdf
                        .continuity_surfaces
                        .values()
                        .filter(|continuity| continuity.prewarmed)
                        .count(),
                ),
            surface_continuity_requests: self.gpu_pdf.surface_continuity_requests,
            surface_continuity_completed: self.gpu_pdf.surface_continuity_completed,
            surface_continuity_hits: self.gpu_pdf.surface_continuity_hits,
            surface_partial_reuses: self.gpu_pdf.surface_partial_reuses,
            surface_presentation_gaps: self.gpu_pdf.surface_presentation_gaps,
            surface_incomplete_suppressed: self.gpu_pdf.surface_incomplete_suppressed,
            surface_refinement_submissions: self.gpu_pdf.surface_refinement_submissions,
            surface_atomic_swaps: self.gpu_pdf.surface_atomic_swaps,
            surface_queue_latency_p95: surface_latency_p95(&self.gpu_pdf.surface_queue_latency),
            surface_ready_latency_p95: surface_latency_p95(&self.gpu_pdf.surface_ready_latency),
            surface_present_latency_p95: surface_latency_p95(&self.gpu_pdf.surface_present_latency),
            refinement_pages: presented_refinement_page_count(
                &self.gpu_pdf.presented_pages,
                self.gpu_pdf.rendered.iter().filter_map(|(page, rendered)| {
                    rendered.stats.refinement_pending.then_some(*page)
                }),
            ),
            retained_refinement_pages: self
                .gpu_pdf
                .rendered
                .values()
                .filter(|rendered| rendered.presentation.refinement_pending())
                .count(),
            active_preparations: self.gpu_pdf.active_preparations(),
            queued_preparations: self.gpu_pdf.queued_preparations(),
            speculative_preparations: self
                .gpu_pdf
                .active_speculative_preparations()
                .saturating_add(self.gpu_pdf.queued_speculative_preparations()),
            completed_preparations: self.gpu_pdf.preparation_completed,
            cancelled_preparations: self.gpu_pdf.preparation_cancelled,
            motion_active: self.gpu_pdf.motion_active,
            zoom_motion_active: self.gpu_pdf.zoom_motion_active,
        }
    }

    pub(in super::super) fn native_gpu_failure(&self, page: usize) -> Option<&str> {
        let page = u32::try_from(page).ok()?;
        if let Some(error) = self.gpu_pdf.surface_failures.get(&page) {
            return Some(error);
        }
        match self.gpu_pdf.pages.get(&page) {
            Some(GpuPageState::Failed(error)) => Some(error),
            None if self.gpu_pdf.terminal_pages.contains(&page) => {
                Some("native PDF page preparation has a cached terminal route")
            }
            _ => self
                .gpu_pdf
                .renderer_error
                .as_deref()
                .or(self.gpu_pdf.document_error.as_deref()),
        }
    }

    pub(in super::super) fn native_gpu_presentation_debug(
        &self,
        page: usize,
    ) -> Option<&GpuPresentationDebugSnapshot> {
        self.gpu_pdf
            .presentation_debug
            .get(&u32::try_from(page).ok()?)
    }

    pub(in super::super) fn native_gpu_vector_lod_failure(&self, page: usize) -> Option<&str> {
        let page = u32::try_from(page).ok()?;
        self.gpu_pdf
            .vector_lod_failures
            .get(&page)
            .map(|failure| failure.message.as_str())
    }

    pub(in super::super) fn native_gpu_pending(&self, page: usize) -> bool {
        let Some(page) = u32::try_from(page).ok() else {
            return false;
        };
        self.gpu_pdf.document_task.is_some()
            || self.gpu_pdf.preparation_demands.contains_key(&page)
            || matches!(
                self.gpu_pdf.pages.get(&page),
                Some(GpuPageState::Preparing { .. })
            )
            || self.gpu_pdf.patch_lod_tasks.contains_key(&page)
            || self.gpu_pdf.vector_lod_tasks.contains_key(&page)
            || self.gpu_pdf.surface_seed_tasks.contains_key(&page)
            || self.gpu_pdf.surface_render_pending.contains_key(&page)
            || self
                .gpu_pdf
                .rendered
                .get(&page)
                .is_some_and(|rendered| rendered.presentation.refinement_pending())
    }
}
