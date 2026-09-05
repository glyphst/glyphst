//! Owned GPU state, cancellation, and bounded cache lifetimes.

use super::*;

#[derive(Default)]
pub(in super::super) struct GpuPdfState {
    pub(super) generation: u64,
    pub(super) document: Option<Arc<GpuPdfDocument>>,
    pub(super) document_task: Option<Task<()>>,
    pub(super) document_error: Option<String>,
    pub(super) navigation_fallback: Option<Arc<[GpuFallbackDiagnostic]>>,
    pub(super) device_token: Option<ExternalGpuDeviceToken>,
    pub(super) renderer: Option<Arc<GpuPdfRenderer>>,
    pub(super) renderer_error: Option<String>,
    pub(super) pages: HashMap<u32, GpuPageState>,
    pub(super) rendered: HashMap<u32, RenderedGpuPage>,
    pub(super) continuity_surfaces: HashMap<u32, ContinuityGpuSurface>,
    pub(super) presented_pages: HashSet<u32>,
    pub(super) presented_continuity_pages: HashSet<u32>,
    pub(super) visible_pages: HashSet<u32>,
    pub(super) covered_pages: HashSet<u32>,
    pub(super) application_fallbacks: HashMap<u32, Arc<[GpuFallbackDiagnostic]>>,
    pub(super) link_mappings: HashMap<u32, CachedLinkMapping>,
    pub(in super::super) reader_link_replacements: HashMap<u32, Vec<(usize, DocumentLink)>>,
    pub(in super::super) reader_original_links: Option<Vec<DocumentLink>>,
    pub(super) surface_failures: HashMap<u32, String>,
    pub(super) direct_renders: HashMap<u32, CachedDirectRender>,
    pub(super) presentation_debug: HashMap<u32, GpuPresentationDebugSnapshot>,
    pub(super) page_last_used: HashMap<u32, u64>,
    pub(super) page_usage_clock: u64,
    pub(super) motion_retained_pages: Vec<u32>,
    pub(super) last_scroll_y_doc: Option<f64>,
    pub(super) last_scroll_direction: i8,
    pub(super) preparation_generation: u64,
    pub(super) preparation_sequence: u64,
    pub(super) preparation_demands: HashMap<u32, GpuPreparationDemand>,
    pub(super) preparation_completed: u64,
    pub(super) preparation_cancelled: u64,
    pub(super) terminal_pages: HashSet<u32>,
    pub(super) patch_lod_generation: u64,
    pub(super) patch_lod_tasks: HashMap<u32, PatchLodTask>,
    pub(super) vector_lod_generation: u64,
    pub(super) vector_lod_tasks: HashMap<u32, VectorLodTask>,
    pub(super) vector_lod_failures: HashMap<u32, VectorLodFailure>,
    pub(super) surface_seed_generation: u64,
    pub(super) surface_seed_tasks: HashMap<u32, SurfaceSeedTask>,
    pub(super) motion_render_inline_frames: u64,
    pub(super) motion_render_inline_failures: u64,
    pub(super) camera_pan_inline_frames: u64,
    pub(super) camera_pan_admission_holds: u64,
    pub(super) motion_camera_serial: u64,
    pub(super) motion_presented_camera_serial: u64,
    pub(super) motion_camera_serial_mismatches: u64,
    pub(super) motion_camera_batch_commits: u64,
    pub(super) surface_render_sequence: u64,
    pub(super) surface_render_pending: HashMap<u32, PendingSurfaceRender>,
    pub(super) surface_render_started: u64,
    pub(super) surface_render_completed: u64,
    pub(super) surface_render_coalesced: u64,
    pub(super) surface_render_superseded: u64,
    pub(super) surface_render_deferred: u64,
    pub(super) surface_render_discarded: u64,
    pub(super) surface_render_cache_pressure: u64,
    pub(super) surface_scroll_reuses: u64,
    pub(super) surface_scroll_prefetches: u64,
    pub(super) surface_scroll_misses: u64,
    pub(super) surface_sharp_pan_cache_presentations: u64,
    pub(super) surface_zoom_reuses: u64,
    pub(super) surface_zoom_quality_refreshes: u64,
    pub(super) surface_zoom_coverage_refreshes: u64,
    pub(super) surface_zoom_settled_upgrades: u64,
    pub(super) surface_zoom_motion_reprojections: u64,
    pub(super) surface_zoom_motion_refreshes_suppressed: u64,
    pub(super) surface_zoom_motion_cancellations: u64,
    pub(super) surface_zoom_emergency_renders: u64,
    pub(super) surface_zoom_live_redraws: u64,
    pub(super) surface_zoom_cached_sharp_presentations: u64,
    pub(super) surface_zoom_live_failures: u64,
    pub(super) surface_zoom_gpu_holds: u64,
    pub(super) surface_zoom_animation_steps: u64,
    pub(super) surface_zoom_admission_holds: u64,
    pub(super) surface_zoom_worker_holds: u64,
    pub(super) surface_zoom_sample_holds: u64,
    pub(super) surface_zoom_presentation_holds: u64,
    pub(super) surface_zoom_scaled_presentations: u64,
    pub(super) surface_zoom_target_reuses: u64,
    pub(super) surface_zoom_target_allocations: u64,
    pub(super) surface_warm_requests: u64,
    pub(super) surface_warm_hits: u64,
    pub(super) surface_continuity_requests: u64,
    pub(super) surface_continuity_completed: u64,
    pub(super) surface_continuity_hits: u64,
    pub(super) surface_partial_reuses: u64,
    pub(super) surface_presentation_gaps: u64,
    pub(super) surface_incomplete_suppressed: u64,
    pub(super) surface_refinement_submissions: u64,
    pub(super) surface_atomic_swaps: u64,
    pub(super) surface_pending_peak: usize,
    pub(super) surface_queue_latency: VecDeque<Duration>,
    pub(super) surface_ready_latency: VecDeque<Duration>,
    pub(super) surface_present_latency: VecDeque<Duration>,
    pub(super) fallback_grace_generation: u64,
    pub(super) fallback_grace_tasks: HashMap<u32, FallbackGraceTask>,
    pub(super) fallback_grace_elapsed: HashSet<u32>,
    pub(super) render_generation: u64,
    pub(super) motion_generation: u64,
    pub(super) motion_active: bool,
    pub(super) zoom_motion_active: bool,
    pub(super) motion_deadline: Option<Instant>,
    pub(super) motion_settle_task: Option<Task<()>>,
    pub(super) speculative_resume_generation: u64,
    pub(super) speculative_resume_task: Option<Task<()>>,
    pub(super) destination_generation: u64,
    pub(super) destination_cancellation: Option<GpuCancellationToken>,
    pub(super) destination_task: Option<Task<()>>,
    pub(super) destination_fallback: Option<Arc<[GpuFallbackDiagnostic]>>,
}

pub(super) enum GpuPageState {
    Preparing {
        generation: u64,
        cancellation: GpuCancellationToken,
        demand: GpuPreparationDemand,
        _task: Task<()>,
    },
    Ready(Arc<PreparedGpuPage>),
    Fallback(Arc<[GpuFallbackDiagnostic]>),
    Failed(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GpuPreparationDemand {
    pub(super) priority: usize,
    pub(super) speculative: bool,
    pub(super) sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GpuPreparationTarget {
    pub(super) page: u32,
    pub(super) priority: usize,
    pub(super) speculative: bool,
}

pub(super) enum CachedLinkMapping {
    Valid(Arc<[(usize, usize)]>),
    Invalid,
}

pub(super) struct RenderedGpuPage {
    pub(super) key: RenderKey,
    pub(super) request: GpuPaintRequest,
    pub(super) refresh_region: GpuPageRegion,
    pub(super) presentation: GpuPagePresentation,
    pub(super) stats: GpuRenderStats,
    pub(super) latest_stats: GpuRenderStats,
    pub(super) enqueued_at: Instant,
    pub(super) first_paint_recorded: bool,
    pub(super) prewarmed: bool,
    pub(super) presented_once: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct DirectRenderIdentity {
    pub(super) key: RenderKey,
    pub(super) visible_region: GpuPageRegion,
    pub(super) device_origin_phase: [f32; 2],
    pub(super) camera_serial: u64,
    pub(super) bounds: Bounds<Pixels>,
}

pub(super) struct CachedDirectRender {
    pub(super) identity: DirectRenderIdentity,
    pub(super) prepared: Arc<PreparedGpuPage>,
    pub(super) paint: GpuDirectPaint,
    pub(super) receipt: GpuDirectRenderReceipt,
    pub(super) reconciled: bool,
}

pub(super) struct ContinuityGpuSurface {
    pub(super) key: RenderKey,
    pub(super) texture: GpuPageTexture,
    pub(super) stats: GpuRenderStats,
    pub(super) enqueued_at: Instant,
    pub(super) first_paint_recorded: bool,
    pub(super) prewarmed: bool,
}

pub(super) struct CurrentGpuSurface {
    pub(super) key: RenderKey,
    pub(super) request: GpuPaintRequest,
    pub(super) refresh_region: GpuPageRegion,
    pub(super) visible_region: GpuPageRegion,
    pub(super) target_region: Option<GpuPageRegion>,
    pub(super) texture: GpuPageTexture,
    pub(super) refinement_pending: bool,
    pub(super) prewarmed: bool,
}

pub(super) struct SelectedGpuSurface {
    pub(super) key: RenderKey,
    pub(super) texture: GpuPageTexture,
    pub(super) kind: PresentedSurfaceKind,
}

pub(super) struct PatchLodTask {
    pub(super) generation: u64,
    pub(super) required_scale: f32,
    pub(super) cancellation: GpuCancellationToken,
    pub(super) _task: Task<()>,
}

pub(super) struct VectorLodTask {
    pub(super) generation: u64,
    pub(super) required_scale: f32,
    pub(super) cancellation: GpuCancellationToken,
    pub(super) _task: Task<()>,
}

pub(super) struct VectorLodFailure {
    pub(super) required_scale: f32,
    pub(super) message: String,
}

pub(super) struct SurfaceSeedTask {
    pub(super) generation: u64,
    pub(super) key: RenderKey,
    pub(super) region: GpuPageRegion,
    pub(super) request: GpuPaintRequest,
    pub(super) role: SurfaceRenderRole,
    pub(super) speculative: bool,
    pub(super) superseded: bool,
    pub(super) enqueued_at: Instant,
    pub(super) chain_started_at: Instant,
    pub(super) started_at: Instant,
    pub(super) in_flight_bytes: u64,
    pub(super) prepared: Arc<PreparedGpuPage>,
    pub(super) renderer: Arc<GpuPdfRenderer>,
    pub(super) _task: Task<()>,
}

pub(super) struct PendingSurfaceRender {
    pub(super) sequence: u64,
    pub(super) key: RenderKey,
    pub(super) region: GpuPageRegion,
    pub(super) prepared: Arc<PreparedGpuPage>,
    pub(super) renderer: Arc<GpuPdfRenderer>,
    pub(super) request: GpuPaintRequest,
    pub(super) role: SurfaceRenderRole,
    pub(super) speculative: bool,
    pub(super) enqueued_at: Instant,
    pub(super) chain_started_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SurfaceRefreshKind {
    Initial,
    Prefetch,
    Urgent,
    ZoomEmergency,
    SettledExact,
    RefinementContinuation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SurfaceRenderRole {
    Primary,
    Continuity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PresentedSurfaceKind {
    Primary,
    Continuity,
    PartialPrimary,
}

#[derive(Clone, Copy)]
pub(super) struct SurfaceRequestIdentity {
    pub(super) key: RenderKey,
    pub(super) request: GpuPaintRequest,
    pub(super) region: GpuPageRegion,
}

impl SurfaceRequestIdentity {
    pub(super) const fn new(
        key: RenderKey,
        request: GpuPaintRequest,
        region: GpuPageRegion,
    ) -> Self {
        Self {
            key,
            request,
            region,
        }
    }
}

pub(super) struct FallbackGraceTask {
    pub(super) generation: u64,
    pub(super) _task: Task<()>,
}

impl GpuPdfState {
    pub(in super::super) fn has_renderer(&self) -> bool {
        self.renderer.is_some()
    }

    pub(in super::super) fn reset(&mut self) {
        self.cancel_page_tasks();
        self.cancel_destination_task();
        self.generation = self.generation.wrapping_add(1);
        self.document = None;
        self.document_task = None;
        self.document_error = None;
        self.navigation_fallback = None;
        self.device_token = None;
        self.renderer = None;
        self.renderer_error = None;
        self.pages.clear();
        self.rendered.clear();
        self.continuity_surfaces.clear();
        self.presented_pages.clear();
        self.presented_continuity_pages.clear();
        self.visible_pages.clear();
        self.covered_pages.clear();
        self.application_fallbacks.clear();
        self.link_mappings.clear();
        self.reader_link_replacements.clear();
        self.reader_original_links = None;
        self.surface_failures.clear();
        self.direct_renders.clear();
        self.presentation_debug.clear();
        self.page_last_used.clear();
        self.page_usage_clock = 0;
        self.motion_retained_pages.clear();
        self.last_scroll_y_doc = None;
        self.last_scroll_direction = 1;
        self.preparation_generation = 0;
        self.preparation_sequence = 0;
        self.preparation_demands.clear();
        self.preparation_completed = 0;
        self.preparation_cancelled = 0;
        self.terminal_pages.clear();
        self.patch_lod_generation = 0;
        self.patch_lod_tasks.clear();
        self.vector_lod_generation = 0;
        self.vector_lod_tasks.clear();
        self.vector_lod_failures.clear();
        self.surface_seed_generation = 0;
        self.surface_seed_tasks.clear();
        self.motion_render_inline_frames = 0;
        self.motion_render_inline_failures = 0;
        self.camera_pan_inline_frames = 0;
        self.camera_pan_admission_holds = 0;
        self.motion_camera_serial = 0;
        self.motion_presented_camera_serial = 0;
        self.motion_camera_serial_mismatches = 0;
        self.motion_camera_batch_commits = 0;
        self.surface_render_sequence = 0;
        self.surface_render_pending.clear();
        self.surface_render_started = 0;
        self.surface_render_completed = 0;
        self.surface_render_coalesced = 0;
        self.surface_render_superseded = 0;
        self.surface_render_deferred = 0;
        self.surface_render_discarded = 0;
        self.surface_render_cache_pressure = 0;
        self.surface_scroll_reuses = 0;
        self.surface_scroll_prefetches = 0;
        self.surface_scroll_misses = 0;
        self.surface_sharp_pan_cache_presentations = 0;
        self.surface_zoom_reuses = 0;
        self.surface_zoom_quality_refreshes = 0;
        self.surface_zoom_coverage_refreshes = 0;
        self.surface_zoom_settled_upgrades = 0;
        self.surface_zoom_motion_reprojections = 0;
        self.surface_zoom_motion_refreshes_suppressed = 0;
        self.surface_zoom_motion_cancellations = 0;
        self.surface_zoom_emergency_renders = 0;
        self.surface_zoom_live_redraws = 0;
        self.surface_zoom_cached_sharp_presentations = 0;
        self.surface_zoom_live_failures = 0;
        self.surface_zoom_gpu_holds = 0;
        self.surface_zoom_animation_steps = 0;
        self.surface_zoom_admission_holds = 0;
        self.surface_zoom_worker_holds = 0;
        self.surface_zoom_sample_holds = 0;
        self.surface_zoom_presentation_holds = 0;
        self.surface_zoom_scaled_presentations = 0;
        self.surface_zoom_target_reuses = 0;
        self.surface_zoom_target_allocations = 0;
        self.surface_warm_requests = 0;
        self.surface_warm_hits = 0;
        self.surface_continuity_requests = 0;
        self.surface_continuity_completed = 0;
        self.surface_continuity_hits = 0;
        self.surface_partial_reuses = 0;
        self.surface_presentation_gaps = 0;
        self.surface_incomplete_suppressed = 0;
        self.surface_refinement_submissions = 0;
        self.surface_atomic_swaps = 0;
        self.surface_pending_peak = 0;
        self.surface_queue_latency.clear();
        self.surface_ready_latency.clear();
        self.surface_present_latency.clear();
        self.fallback_grace_generation = 0;
        self.fallback_grace_tasks.clear();
        self.fallback_grace_elapsed.clear();
        self.render_generation = 0;
        self.motion_generation = self.motion_generation.wrapping_add(1).max(1);
        self.motion_active = false;
        self.zoom_motion_active = false;
        self.motion_deadline = None;
        self.motion_settle_task = None;
        self.speculative_resume_generation =
            self.speculative_resume_generation.wrapping_add(1).max(1);
        self.speculative_resume_task = None;
        self.destination_fallback = None;
    }

    pub(super) fn reset_device(&mut self) {
        self.cancel_page_tasks();
        self.cancel_destination_task();
        self.generation = self.generation.wrapping_add(1);
        self.renderer = None;
        self.renderer_error = None;
        self.pages.clear();
        self.rendered.clear();
        self.continuity_surfaces.clear();
        self.presented_pages.clear();
        self.presented_continuity_pages.clear();
        self.visible_pages.clear();
        self.covered_pages.clear();
        self.application_fallbacks.clear();
        self.link_mappings.clear();
        self.surface_failures.clear();
        self.direct_renders.clear();
        self.presentation_debug.clear();
        self.page_last_used.clear();
        self.page_usage_clock = 0;
        self.motion_retained_pages.clear();
        self.last_scroll_y_doc = None;
        self.last_scroll_direction = 1;
        self.preparation_generation = 0;
        self.preparation_sequence = 0;
        self.preparation_demands.clear();
        self.preparation_completed = 0;
        self.preparation_cancelled = 0;
        self.terminal_pages.clear();
        self.patch_lod_generation = 0;
        self.patch_lod_tasks.clear();
        self.vector_lod_generation = 0;
        self.vector_lod_tasks.clear();
        self.vector_lod_failures.clear();
        self.surface_seed_generation = 0;
        self.surface_seed_tasks.clear();
        self.motion_render_inline_frames = 0;
        self.motion_render_inline_failures = 0;
        self.camera_pan_inline_frames = 0;
        self.camera_pan_admission_holds = 0;
        self.motion_camera_serial = 0;
        self.motion_presented_camera_serial = 0;
        self.motion_camera_serial_mismatches = 0;
        self.motion_camera_batch_commits = 0;
        self.surface_render_sequence = 0;
        self.surface_render_pending.clear();
        self.surface_render_started = 0;
        self.surface_render_completed = 0;
        self.surface_render_coalesced = 0;
        self.surface_render_superseded = 0;
        self.surface_render_deferred = 0;
        self.surface_render_discarded = 0;
        self.surface_render_cache_pressure = 0;
        self.surface_scroll_reuses = 0;
        self.surface_scroll_prefetches = 0;
        self.surface_scroll_misses = 0;
        self.surface_sharp_pan_cache_presentations = 0;
        self.surface_zoom_reuses = 0;
        self.surface_zoom_quality_refreshes = 0;
        self.surface_zoom_coverage_refreshes = 0;
        self.surface_zoom_settled_upgrades = 0;
        self.surface_zoom_motion_reprojections = 0;
        self.surface_zoom_motion_refreshes_suppressed = 0;
        self.surface_zoom_motion_cancellations = 0;
        self.surface_zoom_emergency_renders = 0;
        self.surface_zoom_live_redraws = 0;
        self.surface_zoom_cached_sharp_presentations = 0;
        self.surface_zoom_live_failures = 0;
        self.surface_zoom_gpu_holds = 0;
        self.surface_zoom_animation_steps = 0;
        self.surface_zoom_admission_holds = 0;
        self.surface_zoom_worker_holds = 0;
        self.surface_zoom_sample_holds = 0;
        self.surface_zoom_presentation_holds = 0;
        self.surface_zoom_scaled_presentations = 0;
        self.surface_zoom_target_reuses = 0;
        self.surface_zoom_target_allocations = 0;
        self.surface_warm_requests = 0;
        self.surface_warm_hits = 0;
        self.surface_continuity_requests = 0;
        self.surface_continuity_completed = 0;
        self.surface_continuity_hits = 0;
        self.surface_partial_reuses = 0;
        self.surface_presentation_gaps = 0;
        self.surface_incomplete_suppressed = 0;
        self.surface_refinement_submissions = 0;
        self.surface_atomic_swaps = 0;
        self.surface_pending_peak = 0;
        self.surface_queue_latency.clear();
        self.surface_ready_latency.clear();
        self.surface_present_latency.clear();
        self.fallback_grace_generation = 0;
        self.fallback_grace_tasks.clear();
        self.fallback_grace_elapsed.clear();
        self.render_generation = 0;
        self.motion_generation = self.motion_generation.wrapping_add(1).max(1);
        self.motion_active = false;
        self.zoom_motion_active = false;
        self.motion_deadline = None;
        self.motion_settle_task = None;
        self.speculative_resume_generation =
            self.speculative_resume_generation.wrapping_add(1).max(1);
        self.speculative_resume_task = None;
        self.destination_fallback = None;
    }

    pub(super) fn cancel_page_tasks(&mut self) {
        for state in self.pages.values() {
            if let GpuPageState::Preparing { cancellation, .. } = state {
                cancellation.cancel();
            }
        }
        for task in self.patch_lod_tasks.values() {
            task.cancellation.cancel();
        }
        for task in self.vector_lod_tasks.values() {
            task.cancellation.cancel();
        }
        self.patch_lod_tasks.clear();
        self.vector_lod_tasks.clear();
        self.render_generation = self.render_generation.wrapping_add(1).max(1);
        for task in self.surface_seed_tasks.values() {
            _ = task
                .renderer
                .supersede_page_render(&task.prepared, self.render_generation);
        }
        self.surface_seed_tasks.clear();
        self.speculative_resume_generation =
            self.speculative_resume_generation.wrapping_add(1).max(1);
        self.speculative_resume_task = None;
        self.surface_render_pending.clear();
        self.fallback_grace_tasks.clear();
    }

    pub(super) fn begin_motion(&mut self) {
        if self.motion_active {
            return;
        }
        let speculative_preparations = self
            .pages
            .iter()
            .filter_map(|(page, state)| {
                matches!(
                    state,
                    GpuPageState::Preparing {
                        demand: GpuPreparationDemand {
                            speculative: true,
                            ..
                        },
                        ..
                    }
                )
                .then_some(*page)
            })
            .collect::<Vec<_>>();
        for page in speculative_preparations {
            self.cancel_active_preparation(page, true);
        }

        self.surface_render_pending.retain(|_, pending| {
            !pending.speculative && pending.role == SurfaceRenderRole::Primary
        });
        let active = self
            .surface_seed_tasks
            .iter()
            .filter(|(_, task)| {
                !task.superseded && (task.speculative || task.role == SurfaceRenderRole::Continuity)
            })
            .map(|(page, task)| {
                (
                    *page,
                    Arc::clone(&task.prepared),
                    Arc::clone(&task.renderer),
                )
            })
            .collect::<Vec<_>>();
        if !active.is_empty() {
            self.render_generation = self.render_generation.wrapping_add(1).max(1);
            for (page, prepared, renderer) in active {
                if renderer
                    .supersede_page_render(&prepared, self.render_generation)
                    .is_ok()
                    && let Some(task) = self.surface_seed_tasks.get_mut(&page)
                {
                    task.superseded = true;
                }
            }
        }
    }

    pub(super) fn begin_zoom_motion(&mut self) {
        if self.zoom_motion_active {
            return;
        }
        self.zoom_motion_active = true;

        // GPU page preparation uploads buffers on the same WGPU device used
        // by live redraw. Even though compilation runs on a worker, a
        // speculative upload can take the driver's device lock long enough to
        // create a visible zoom hitch. Preserve its demand, but cancel every
        // speculative worker until the gesture settles.
        let speculative_preparations = self
            .pages
            .iter()
            .filter_map(|(page, state)| {
                matches!(
                    state,
                    GpuPageState::Preparing {
                        demand: GpuPreparationDemand {
                            speculative: true,
                            ..
                        },
                        ..
                    }
                )
                .then_some(*page)
            })
            .collect::<Vec<_>>();
        for page in &speculative_preparations {
            self.cancel_active_preparation(*page, true);
        }

        let coherent_pages = self
            .rendered
            .keys()
            .chain(self.continuity_surfaces.keys())
            .copied()
            .collect::<HashSet<_>>();
        let pending_before = self.surface_render_pending.len();
        self.surface_render_pending.retain(|page, pending| {
            !zoom_motion_cancels_surface_work(
                coherent_pages.contains(page),
                pending.role,
                pending.speculative,
            )
        });
        let cancelled_pending = pending_before.saturating_sub(self.surface_render_pending.len());

        let stale_patch_lods = self
            .patch_lod_tasks
            .keys()
            .filter(|page| coherent_pages.contains(page))
            .copied()
            .collect::<Vec<_>>();
        for page in stale_patch_lods {
            if let Some(task) = self.patch_lod_tasks.remove(&page) {
                task.cancellation.cancel();
            }
        }
        let stale_vector_lods = self
            .vector_lod_tasks
            .keys()
            .filter(|page| coherent_pages.contains(page))
            .copied()
            .collect::<Vec<_>>();
        for page in stale_vector_lods {
            if let Some(task) = self.vector_lod_tasks.remove(&page) {
                task.cancellation.cancel();
            }
        }

        let active = self
            .surface_seed_tasks
            .iter()
            .filter(|(page, task)| {
                !task.superseded
                    && zoom_motion_cancels_surface_work(
                        coherent_pages.contains(page),
                        task.role,
                        task.speculative,
                    )
            })
            .map(|(page, task)| {
                (
                    *page,
                    Arc::clone(&task.prepared),
                    Arc::clone(&task.renderer),
                )
            })
            .collect::<Vec<_>>();
        let mut cancelled_active = 0_usize;
        if !active.is_empty() {
            self.render_generation = self.render_generation.wrapping_add(1).max(1);
            for (page, prepared, renderer) in active {
                if renderer
                    .supersede_page_render(&prepared, self.render_generation)
                    .is_ok()
                {
                    if let Some(task) = self.surface_seed_tasks.get_mut(&page) {
                        task.superseded = true;
                    }
                    cancelled_active = cancelled_active.saturating_add(1);
                }
            }
        }
        self.surface_zoom_motion_cancellations =
            self.surface_zoom_motion_cancellations.saturating_add(
                (cancelled_pending
                    .saturating_add(cancelled_active)
                    .saturating_add(speculative_preparations.len())) as u64,
            );
    }

    pub(super) fn cancel_destination_task(&mut self) {
        if let Some(cancellation) = self.destination_cancellation.take() {
            cancellation.cancel();
        }
        self.destination_task = None;
        self.destination_generation = self.destination_generation.wrapping_add(1).max(1);
    }

    pub(super) fn is_active(&self) -> bool {
        self.document.is_some()
            || self.document_task.is_some()
            || self.document_error.is_some()
            || self.navigation_fallback.is_some()
            || self.device_token.is_some()
            || self.renderer.is_some()
            || self.renderer_error.is_some()
            || self.destination_task.is_some()
            || self.destination_fallback.is_some()
            || self.reader_original_links.is_some()
            || !self.patch_lod_tasks.is_empty()
            || !self.vector_lod_tasks.is_empty()
            || !self.surface_seed_tasks.is_empty()
            || self.speculative_resume_task.is_some()
            || !self.surface_render_pending.is_empty()
            || !self.preparation_demands.is_empty()
            || !self.terminal_pages.is_empty()
            || !self.pages.is_empty()
            || !self.rendered.is_empty()
            || !self.continuity_surfaces.is_empty()
            || !self.direct_renders.is_empty()
    }

    pub(super) fn mark_page_used(&mut self, page: u32) {
        self.page_usage_clock = self.page_usage_clock.wrapping_add(1).max(1);
        self.page_last_used.insert(page, self.page_usage_clock);
    }

    pub(super) fn record_surface_latency(&mut self, queue: Duration, ready: Duration) {
        push_surface_latency(&mut self.surface_queue_latency, queue);
        push_surface_latency(&mut self.surface_ready_latency, ready);
    }

    pub(super) fn record_surface_present_latency(&mut self, present: Duration) {
        push_surface_latency(&mut self.surface_present_latency, present);
    }

    pub(super) fn prune(&mut self, visible: &HashSet<u32>, retained: &HashSet<u32>) {
        self.visible_pages.clone_from(visible);
        self.presented_pages.clear();
        self.presented_continuity_pages.clear();
        self.covered_pages.clear();
        self.presentation_debug.clear();
        let stale_patch_lods = self
            .patch_lod_tasks
            .keys()
            .filter(|page| !visible.contains(page))
            .copied()
            .collect::<Vec<_>>();
        for page in stale_patch_lods {
            if let Some(task) = self.patch_lod_tasks.remove(&page) {
                task.cancellation.cancel();
            }
        }
        let stale_vector_lods = self
            .vector_lod_tasks
            .keys()
            .filter(|page| !visible.contains(page))
            .copied()
            .collect::<Vec<_>>();
        for page in stale_vector_lods {
            if let Some(task) = self.vector_lod_tasks.remove(&page) {
                task.cancellation.cancel();
            }
        }
        let stale_surface_task = self
            .surface_seed_tasks
            .iter()
            .find(|(page, task)| !retained.contains(page) && !task.superseded)
            .map(|(page, _)| *page);
        if let Some(page) = stale_surface_task {
            self.render_generation = self.render_generation.wrapping_add(1).max(1);
            if let Some(task) = self.surface_seed_tasks.get_mut(&page)
                && task
                    .renderer
                    .supersede_page_render(&task.prepared, self.render_generation)
                    .is_ok()
            {
                task.superseded = true;
            }
        }
        self.surface_render_pending
            .retain(|page, _| retained.contains(page));
        self.fallback_grace_tasks
            .retain(|page, _| retained.contains(page));
        self.fallback_grace_elapsed
            .retain(|page| retained.contains(page));
        let stale_preparations = self
            .pages
            .iter()
            .filter_map(|(page, state)| {
                (!retained.contains(page) && matches!(state, GpuPageState::Preparing { .. }))
                    .then_some(*page)
            })
            .collect::<Vec<_>>();
        for page in stale_preparations {
            if let Some(GpuPageState::Preparing { cancellation, .. }) = self.pages.remove(&page) {
                cancellation.cancel();
                self.preparation_cancelled = self.preparation_cancelled.saturating_add(1);
            }
            self.preparation_demands.remove(&page);
            self.page_last_used.remove(&page);
            self.rendered.remove(&page);
            self.continuity_surfaces.remove(&page);
            self.presented_pages.remove(&page);
            self.application_fallbacks.remove(&page);
            self.link_mappings.remove(&page);
            self.surface_failures.remove(&page);
            self.vector_lod_failures.remove(&page);
        }

        // Reserve capacity only for pages that must be painted now. Merely
        // predicting a new speculative corridor must not synchronously evict
        // retained WGPU resources from the old corridor during a direction
        // reversal; speculative preparation will naturally wait while the
        // cache is full.
        let incoming_visible = visible
            .iter()
            .filter(|page| !self.pages.contains_key(page))
            .count();
        let retained_limit = MAX_RETAINED_GPU_PAGES.saturating_sub(incoming_visible);
        self.prune_states(retained, retained_limit, |state| {
            matches!(state, GpuPageState::Ready(_))
        });
        self.prune_states(retained, MAX_RETAINED_GPU_DIAGNOSTICS, |state| {
            matches!(state, GpuPageState::Fallback(_) | GpuPageState::Failed(_))
        });
        self.prune_prepared_resource_cache(visible);
        self.rendered
            .retain(|page, _| self.pages.contains_key(page));
        self.continuity_surfaces
            .retain(|page, _| self.pages.contains_key(page));
        self.page_last_used
            .retain(|page, _| self.pages.contains_key(page));
        self.application_fallbacks
            .retain(|page, _| self.pages.contains_key(page));
        self.link_mappings
            .retain(|page, _| self.pages.contains_key(page));
        self.surface_failures
            .retain(|page, _| self.pages.contains_key(page));
        self.direct_renders
            .retain(|page, _| retained.contains(page));
        self.vector_lod_failures
            .retain(|page, _| self.pages.contains_key(page));
        let evicted_patch_lods = self
            .patch_lod_tasks
            .keys()
            .filter(|page| !self.pages.contains_key(page))
            .copied()
            .collect::<Vec<_>>();
        for page in evicted_patch_lods {
            if let Some(task) = self.patch_lod_tasks.remove(&page) {
                task.cancellation.cancel();
            }
        }
        let evicted_vector_lods = self
            .vector_lod_tasks
            .keys()
            .filter(|page| !self.pages.contains_key(page))
            .copied()
            .collect::<Vec<_>>();
        for page in evicted_vector_lods {
            if let Some(task) = self.vector_lod_tasks.remove(&page) {
                task.cancellation.cancel();
            }
        }
        self.prune_surface_cache();
    }

    pub(super) fn prune_states(
        &mut self,
        visible: &HashSet<u32>,
        limit: usize,
        matches_state: impl Fn(&GpuPageState) -> bool,
    ) {
        let count = self
            .pages
            .values()
            .filter(|state| matches_state(state))
            .count();
        let mut candidates = self
            .pages
            .iter()
            .filter_map(|(page, state)| {
                (!visible.contains(page) && matches_state(state)).then_some((
                    *page,
                    self.page_last_used.get(page).copied().unwrap_or_default(),
                ))
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|(_, last_used)| *last_used);
        for (page, _) in candidates.into_iter().take(count.saturating_sub(limit)) {
            self.pages.remove(&page);
            self.page_last_used.remove(&page);
            self.rendered.remove(&page);
            self.continuity_surfaces.remove(&page);
            self.presented_pages.remove(&page);
            self.application_fallbacks.remove(&page);
            self.link_mappings.remove(&page);
            self.surface_failures.remove(&page);
            self.vector_lod_failures.remove(&page);
        }
    }

    pub(super) fn surface_bytes(&self) -> u64 {
        let primary = self
            .rendered
            .values()
            .map(|rendered| rendered.presentation.byte_len())
            .sum::<u64>();
        primary.saturating_add(
            self.continuity_surfaces
                .values()
                .map(|continuity| continuity.texture.byte_len())
                .sum::<u64>(),
        )
    }

    pub(super) fn prepared_resource_bytes(&self) -> u64 {
        self.pages
            .values()
            .filter_map(|state| match state {
                GpuPageState::Ready(page) => Some(
                    page.preparation_stats()
                        .resident_bytes
                        .saturating_add(page.patch_lod_preparation_stats().mesh_bytes),
                ),
                _ => None,
            })
            .sum()
    }

    pub(super) fn prune_prepared_resource_cache(&mut self, visible: &HashSet<u32>) {
        while self.prepared_resource_bytes() > MAX_RETAINED_GPU_RESOURCE_BYTES {
            let Some(page) =
                self.pages
                    .iter()
                    .filter_map(|(page, state)| {
                        (!visible.contains(page) && matches!(state, GpuPageState::Ready(_)))
                            .then_some((
                                *page,
                                self.page_last_used.get(page).copied().unwrap_or_default(),
                            ))
                    })
                    .min_by_key(|(_, last_used)| *last_used)
                    .map(|(page, _)| page)
            else {
                break;
            };
            self.pages.remove(&page);
            self.page_last_used.remove(&page);
            self.rendered.remove(&page);
            self.continuity_surfaces.remove(&page);
            self.presented_pages.remove(&page);
            self.application_fallbacks.remove(&page);
            self.link_mappings.remove(&page);
            self.surface_failures.remove(&page);
        }
    }

    pub(super) fn prune_surface_cache(&mut self) {
        while self.surface_bytes() > MAX_RETAINED_GPU_SURFACE_BYTES {
            if let Some(page) = self
                .continuity_surfaces
                .keys()
                .filter(|page| !self.visible_pages.contains(page))
                .min_by_key(|page| self.page_last_used.get(page).copied().unwrap_or_default())
                .copied()
            {
                self.continuity_surfaces.remove(&page);
                continue;
            }
            let Some(page) = self
                .rendered
                .keys()
                .filter(|page| !self.visible_pages.contains(page))
                .min_by_key(|page| self.page_last_used.get(page).copied().unwrap_or_default())
                .copied()
            else {
                break;
            };
            self.rendered.remove(&page);
        }
    }

    pub(super) fn reserve_surface(&mut self, page: u32, bytes: u64) -> Result<(), u64> {
        loop {
            let retained_primary = self
                .rendered
                .iter()
                .filter(|(candidate, _)| **candidate != page)
                .map(|(_, rendered)| rendered.presentation.byte_len())
                .sum::<u64>();
            let retained_continuity = self
                .continuity_surfaces
                .values()
                .map(|continuity| continuity.texture.byte_len())
                .sum::<u64>();
            let retained = retained_primary.saturating_add(retained_continuity);
            if retained.saturating_add(bytes) <= MAX_RETAINED_GPU_SURFACE_BYTES {
                return Ok(());
            }
            if let Some(candidate) = self
                .continuity_surfaces
                .keys()
                .filter(|candidate| **candidate != page && !self.visible_pages.contains(candidate))
                .min_by_key(|candidate| {
                    self.page_last_used
                        .get(candidate)
                        .copied()
                        .unwrap_or_default()
                })
                .copied()
            {
                self.continuity_surfaces.remove(&candidate);
                continue;
            }
            let page_continuity_bytes = self
                .continuity_surfaces
                .get(&page)
                .map_or(0, |continuity| continuity.texture.byte_len());
            if page_continuity_bytes > 0
                && retained
                    .saturating_sub(page_continuity_bytes)
                    .saturating_add(bytes)
                    <= MAX_RETAINED_GPU_SURFACE_BYTES
            {
                // The new primary is already complete. If its only obstacle
                // is this page's transition anchor, replace the anchor rather
                // than rejecting the higher-quality surface.
                self.continuity_surfaces.remove(&page);
                continue;
            }
            let Some(candidate) = self
                .rendered
                .keys()
                .filter(|candidate| **candidate != page && !self.visible_pages.contains(candidate))
                .min_by_key(|candidate| {
                    self.page_last_used
                        .get(candidate)
                        .copied()
                        .unwrap_or_default()
                })
                .copied()
            else {
                return Err(retained.saturating_add(bytes));
            };
            self.rendered.remove(&candidate);
        }
    }

    pub(super) fn reserve_continuity_surface(&mut self, page: u32, bytes: u64) -> bool {
        loop {
            let retained = self.surface_bytes().saturating_sub(
                self.continuity_surfaces
                    .get(&page)
                    .map_or(0, |continuity| continuity.texture.byte_len()),
            );
            if retained.saturating_add(bytes) <= MAX_RETAINED_GPU_SURFACE_BYTES {
                return true;
            }
            let Some(candidate) = self
                .continuity_surfaces
                .keys()
                .filter(|candidate| **candidate != page && !self.visible_pages.contains(candidate))
                .min_by_key(|candidate| {
                    self.page_last_used
                        .get(candidate)
                        .copied()
                        .unwrap_or_default()
                })
                .copied()
            else {
                return false;
            };
            self.continuity_surfaces.remove(&candidate);
        }
    }

    pub(super) fn active_preparations(&self) -> usize {
        self.pages
            .values()
            .filter(|state| matches!(state, GpuPageState::Preparing { .. }))
            .count()
    }

    pub(super) fn active_speculative_preparations(&self) -> usize {
        self.pages
            .values()
            .filter(|state| {
                matches!(
                    state,
                    GpuPageState::Preparing {
                        demand: GpuPreparationDemand {
                            speculative: true,
                            ..
                        },
                        ..
                    }
                )
            })
            .count()
    }

    pub(super) fn worst_speculative_preparation(&self) -> Option<u32> {
        self.pages
            .iter()
            .filter_map(|(page, state)| match state {
                GpuPageState::Preparing {
                    demand:
                        demand @ GpuPreparationDemand {
                            speculative: true, ..
                        },
                    ..
                } => Some((*page, *demand)),
                _ => None,
            })
            .max_by_key(|(page, demand)| (demand.priority, demand.sequence, *page))
            .map(|(page, _)| page)
    }

    pub(super) fn queued_preparations(&self) -> usize {
        self.preparation_demands
            .keys()
            .filter(|page| !self.pages.contains_key(page))
            .count()
    }

    pub(super) fn queued_speculative_preparations(&self) -> usize {
        self.preparation_demands
            .iter()
            .filter(|(page, demand)| demand.speculative && !self.pages.contains_key(page))
            .count()
    }

    pub(super) fn set_preparation_demands(&mut self, targets: &[GpuPreparationTarget]) {
        let desired = targets
            .iter()
            .map(|target| target.page)
            .collect::<HashSet<_>>();
        let dropped = self
            .preparation_demands
            .keys()
            .filter(|page| !desired.contains(page))
            .count() as u64;
        self.preparation_cancelled = self.preparation_cancelled.saturating_add(dropped);
        self.preparation_demands
            .retain(|page, _| desired.contains(page));

        for target in targets {
            if self.terminal_pages.contains(&target.page)
                || matches!(
                    self.pages.get(&target.page),
                    Some(
                        GpuPageState::Ready(_)
                            | GpuPageState::Fallback(_)
                            | GpuPageState::Failed(_)
                    )
                )
            {
                self.preparation_demands.remove(&target.page);
                continue;
            }
            let sequence = if let Some(demand) = self.preparation_demands.get(&target.page) {
                demand.sequence
            } else {
                self.preparation_sequence = self.preparation_sequence.wrapping_add(1).max(1);
                self.preparation_sequence
            };
            let demand = GpuPreparationDemand {
                priority: target.priority,
                speculative: target.speculative,
                sequence,
            };
            self.preparation_demands.insert(target.page, demand);
            if let Some(GpuPageState::Preparing { demand: active, .. }) =
                self.pages.get_mut(&target.page)
            {
                *active = demand;
            }
        }
    }

    pub(super) fn demand_page(&mut self, page: u32, speculative: bool, priority: usize) {
        if self.terminal_pages.contains(&page)
            || matches!(
                self.pages.get(&page),
                Some(GpuPageState::Ready(_) | GpuPageState::Fallback(_) | GpuPageState::Failed(_))
            )
        {
            return;
        }
        let sequence = if let Some(demand) = self.preparation_demands.get(&page) {
            demand.sequence
        } else {
            self.preparation_sequence = self.preparation_sequence.wrapping_add(1).max(1);
            self.preparation_sequence
        };
        let demand = GpuPreparationDemand {
            priority,
            speculative,
            sequence,
        };
        self.preparation_demands.insert(page, demand);
        if let Some(GpuPageState::Preparing { demand: active, .. }) = self.pages.get_mut(&page) {
            *active = demand;
        }
    }

    pub(super) fn cancel_active_preparation(&mut self, page: u32, retain_demand: bool) {
        if let Some(GpuPageState::Preparing { cancellation, .. }) = self.pages.remove(&page) {
            cancellation.cancel();
            self.preparation_cancelled = self.preparation_cancelled.saturating_add(1);
        }
        if !retain_demand {
            self.preparation_demands.remove(&page);
        }
    }

    pub(in super::super) fn invalidate_link_mappings(&mut self) {
        self.link_mappings.clear();
        self.application_fallbacks.clear();
    }
}

impl Drop for GpuPdfState {
    fn drop(&mut self) {
        self.cancel_page_tasks();
    }
}
