use super::*;
use std::collections::VecDeque;

const MAX_RETAINED_GPU_PAGES: usize = 8;
const MAX_RETAINED_GPU_DIAGNOSTICS: usize = 64;
const MAX_CONCURRENT_GPU_PAGE_PREPARATIONS: usize = 2;
const MAX_RETAINED_GPU_RESOURCE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RETAINED_GPU_SURFACE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SURFACE_LATENCY_SAMPLES: usize = 120;
const GPU_CONTINUITY_LONG_EDGE: u32 = 2_048;
const GPU_TARGET_ALIGNMENT: u32 = 128;
const GPU_MOTION_MIN_DENSITY_NUMERATOR: u64 = 9;
const GPU_MOTION_MIN_DENSITY_DENOMINATOR: u64 = 10;

const GPU_MOTION_SETTLE_DELAY: Duration = Duration::from_millis(100);
const GPU_SPECULATIVE_RESUME_DELAY: Duration = Duration::from_millis(400);
const GPU_FIRST_PAINT_GRACE: Duration = Duration::from_millis(50);

fn same_patch_lod_request(existing: f32, required: f32) -> bool {
    existing.to_bits() == required.to_bits()
}

fn next_vector_lod_scale(available: f32, requested: f64) -> Option<f32> {
    if !available.is_finite()
        || available < 1.0
        || !requested.is_finite()
        || requested <= f64::from(available) * 1.000_001
    {
        return None;
    }
    // Reader zoom is capped independently, but the page-to-device transform
    // also includes fit scale and display density. Refinement must therefore
    // follow the measured device scale instead of clipping it to the camera
    // zoom limit.
    let requested = requested.min(f64::from(f32::MAX)) as f32;
    let doubled = available * 2.0;
    let next = if doubled.is_finite() {
        doubled.min(requested)
    } else {
        requested
    };
    (next.is_finite() && next > available).then_some(next)
}

fn presented_refinement_page_count(
    presented_pages: &HashSet<u32>,
    pending_pages: impl IntoIterator<Item = u32>,
) -> usize {
    pending_pages
        .into_iter()
        .filter(|page| presented_pages.contains(page))
        .count()
}

fn replace_queued_motion<T>(queued: &mut VecDeque<T>, latest: T) -> usize {
    let replaced = queued.len();
    queued.clear();
    queued.push_back(latest);
    replaced
}

#[derive(Default)]
pub(super) struct GpuPdfState {
    generation: u64,
    document: Option<Arc<GpuPdfDocument>>,
    document_task: Option<Task<()>>,
    document_error: Option<String>,
    navigation_fallback: Option<Arc<[GpuFallbackDiagnostic]>>,
    device_token: Option<ExternalGpuDeviceToken>,
    renderer: Option<Arc<GpuPdfRenderer>>,
    renderer_error: Option<String>,
    pages: HashMap<u32, GpuPageState>,
    rendered: HashMap<u32, RenderedGpuPage>,
    continuity_surfaces: HashMap<u32, ContinuityGpuSurface>,
    presented_pages: HashSet<u32>,
    presented_continuity_pages: HashSet<u32>,
    visible_pages: HashSet<u32>,
    covered_pages: HashSet<u32>,
    application_fallbacks: HashMap<u32, Arc<[GpuFallbackDiagnostic]>>,
    link_mappings: HashMap<u32, CachedLinkMapping>,
    pub(super) reader_link_replacements: HashMap<u32, Vec<(usize, DocumentLink)>>,
    pub(super) reader_original_links: Option<Vec<DocumentLink>>,
    surface_failures: HashMap<u32, String>,
    page_last_used: HashMap<u32, u64>,
    page_usage_clock: u64,
    motion_retained_pages: Vec<u32>,
    last_scroll_y_doc: Option<f32>,
    last_scroll_direction: i8,
    preparation_generation: u64,
    preparation_sequence: u64,
    preparation_demands: HashMap<u32, GpuPreparationDemand>,
    preparation_completed: u64,
    preparation_cancelled: u64,
    terminal_pages: HashSet<u32>,
    patch_lod_generation: u64,
    patch_lod_tasks: HashMap<u32, PatchLodTask>,
    vector_lod_generation: u64,
    vector_lod_tasks: HashMap<u32, VectorLodTask>,
    vector_lod_failures: HashMap<u32, VectorLodFailure>,
    surface_seed_generation: u64,
    surface_seed_tasks: HashMap<u32, SurfaceSeedTask>,
    motion_render_generation: u64,
    motion_render_tasks: HashMap<u32, MotionRenderTask>,
    motion_render_queued: HashMap<u32, VecDeque<QueuedMotionRender>>,
    motion_target_reservations: HashMap<u32, GpuMotionTargetReservation>,
    motion_render_started: u64,
    motion_render_completed: u64,
    motion_render_superseded: u64,
    motion_render_detached_reuses: u64,
    motion_render_peak: usize,
    motion_reservations_started: u64,
    motion_reservations_consumed: u64,
    motion_reservations_released: u64,
    surface_render_sequence: u64,
    surface_render_pending: HashMap<u32, PendingSurfaceRender>,
    surface_render_started: u64,
    surface_render_completed: u64,
    surface_render_coalesced: u64,
    surface_render_superseded: u64,
    surface_render_deferred: u64,
    surface_render_discarded: u64,
    surface_render_cache_pressure: u64,
    surface_scroll_reuses: u64,
    surface_scroll_prefetches: u64,
    surface_scroll_misses: u64,
    surface_zoom_reuses: u64,
    surface_zoom_quality_refreshes: u64,
    surface_zoom_coverage_refreshes: u64,
    surface_zoom_settled_upgrades: u64,
    surface_zoom_motion_reprojections: u64,
    surface_zoom_motion_refreshes_suppressed: u64,
    surface_zoom_motion_cancellations: u64,
    surface_zoom_emergency_renders: u64,
    surface_zoom_live_redraws: u64,
    surface_zoom_cached_sharp_presentations: u64,
    surface_zoom_live_failures: u64,
    surface_zoom_gpu_holds: u64,
    surface_zoom_animation_steps: u64,
    surface_zoom_admission_holds: u64,
    surface_zoom_worker_holds: u64,
    surface_zoom_sample_holds: u64,
    surface_zoom_presentation_holds: u64,
    surface_zoom_scaled_presentations: u64,
    surface_zoom_target_reuses: u64,
    surface_zoom_target_allocations: u64,
    surface_warm_requests: u64,
    surface_warm_hits: u64,
    surface_continuity_requests: u64,
    surface_continuity_completed: u64,
    surface_continuity_hits: u64,
    surface_partial_reuses: u64,
    surface_presentation_gaps: u64,
    surface_incomplete_suppressed: u64,
    surface_refinement_submissions: u64,
    surface_atomic_swaps: u64,
    surface_pending_peak: usize,
    surface_queue_latency: VecDeque<Duration>,
    surface_ready_latency: VecDeque<Duration>,
    surface_present_latency: VecDeque<Duration>,
    fallback_grace_generation: u64,
    fallback_grace_tasks: HashMap<u32, FallbackGraceTask>,
    fallback_grace_elapsed: HashSet<u32>,
    render_generation: u64,
    motion_generation: u64,
    motion_active: bool,
    zoom_motion_active: bool,
    motion_deadline: Option<Instant>,
    motion_settle_task: Option<Task<()>>,
    speculative_resume_generation: u64,
    speculative_resume_task: Option<Task<()>>,
    destination_generation: u64,
    destination_cancellation: Option<GpuCancellationToken>,
    destination_task: Option<Task<()>>,
    destination_fallback: Option<Arc<[GpuFallbackDiagnostic]>>,
}

enum GpuPageState {
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
struct GpuPreparationDemand {
    priority: usize,
    speculative: bool,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GpuPreparationTarget {
    page: u32,
    priority: usize,
    speculative: bool,
}

enum CachedLinkMapping {
    Valid(Arc<[(usize, usize)]>),
    Invalid,
}

struct RenderedGpuPage {
    key: RenderKey,
    request: GpuPaintRequest,
    refresh_region: GpuPageRegion,
    presentation: GpuPagePresentation,
    stats: GpuRenderStats,
    latest_stats: GpuRenderStats,
    enqueued_at: Instant,
    first_paint_recorded: bool,
    prewarmed: bool,
    last_paint: Option<GpuPaintTexture>,
}

struct ContinuityGpuSurface {
    key: RenderKey,
    texture: GpuPageTexture,
    stats: GpuRenderStats,
    enqueued_at: Instant,
    first_paint_recorded: bool,
    prewarmed: bool,
}

struct CurrentGpuSurface {
    key: RenderKey,
    request: GpuPaintRequest,
    refresh_region: GpuPageRegion,
    visible_region: GpuPageRegion,
    target_region: Option<GpuPageRegion>,
    texture: GpuPageTexture,
    refinement_pending: bool,
    prewarmed: bool,
}

struct SelectedGpuSurface {
    key: RenderKey,
    texture: GpuPageTexture,
    kind: PresentedSurfaceKind,
}

struct PatchLodTask {
    generation: u64,
    required_scale: f32,
    cancellation: GpuCancellationToken,
    _task: Task<()>,
}

struct VectorLodTask {
    generation: u64,
    required_scale: f32,
    cancellation: GpuCancellationToken,
    _task: Task<()>,
}

struct VectorLodFailure {
    required_scale: f32,
    message: String,
}

struct SurfaceSeedTask {
    generation: u64,
    key: RenderKey,
    region: GpuPageRegion,
    request: GpuPaintRequest,
    role: SurfaceRenderRole,
    speculative: bool,
    superseded: bool,
    enqueued_at: Instant,
    chain_started_at: Instant,
    started_at: Instant,
    in_flight_bytes: u64,
    prepared: Arc<PreparedGpuPage>,
    renderer: Arc<GpuPdfRenderer>,
    _task: Task<()>,
}

struct MotionRenderTask {
    page: u32,
    generation: u64,
    prepared: Arc<PreparedGpuPage>,
    renderer: Arc<GpuPdfRenderer>,
    _task: Task<()>,
}

struct QueuedMotionRender {
    page: u32,
    generation: u64,
    key: RenderKey,
    request: GpuPaintRequest,
    presentation: GpuPagePresentation,
    previous_surface_bytes: u64,
    prepared: Arc<PreparedGpuPage>,
    renderer: Arc<GpuPdfRenderer>,
    reservation: GpuMotionTargetReservation,
    enqueued_at: Instant,
}

struct PendingSurfaceRender {
    sequence: u64,
    key: RenderKey,
    region: GpuPageRegion,
    prepared: Arc<PreparedGpuPage>,
    renderer: Arc<GpuPdfRenderer>,
    request: GpuPaintRequest,
    role: SurfaceRenderRole,
    speculative: bool,
    enqueued_at: Instant,
    chain_started_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurfaceRefreshKind {
    Initial,
    Prefetch,
    Urgent,
    ZoomEmergency,
    SettledExact,
    RefinementContinuation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurfaceRenderRole {
    Primary,
    Continuity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PresentedSurfaceKind {
    Primary,
    Continuity,
    PartialPrimary,
}

#[derive(Clone, Copy)]
struct SurfaceRequestIdentity {
    key: RenderKey,
    request: GpuPaintRequest,
    region: GpuPageRegion,
}

impl SurfaceRequestIdentity {
    const fn new(key: RenderKey, request: GpuPaintRequest, region: GpuPageRegion) -> Self {
        Self {
            key,
            request,
            region,
        }
    }
}

struct FallbackGraceTask {
    generation: u64,
    _task: Task<()>,
}

impl GpuPdfState {
    pub(super) fn reset(&mut self) {
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
        self.motion_render_generation = self.motion_render_generation.wrapping_add(1).max(1);
        self.motion_render_tasks.clear();
        self.motion_render_queued.clear();
        self.motion_target_reservations.clear();
        self.motion_render_started = 0;
        self.motion_render_completed = 0;
        self.motion_render_superseded = 0;
        self.motion_render_detached_reuses = 0;
        self.motion_render_peak = 0;
        self.motion_reservations_started = 0;
        self.motion_reservations_consumed = 0;
        self.motion_reservations_released = 0;
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

    fn reset_device(&mut self) {
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
        self.motion_render_generation = self.motion_render_generation.wrapping_add(1).max(1);
        self.motion_render_tasks.clear();
        self.motion_render_queued.clear();
        self.motion_target_reservations.clear();
        self.motion_render_started = 0;
        self.motion_render_completed = 0;
        self.motion_render_superseded = 0;
        self.motion_render_detached_reuses = 0;
        self.motion_render_peak = 0;
        self.motion_reservations_started = 0;
        self.motion_reservations_consumed = 0;
        self.motion_reservations_released = 0;
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

    fn cancel_page_tasks(&mut self) {
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
        self.cancel_motion_render_tasks();
        self.speculative_resume_generation =
            self.speculative_resume_generation.wrapping_add(1).max(1);
        self.speculative_resume_task = None;
        self.surface_render_pending.clear();
        self.fallback_grace_tasks.clear();
    }

    fn cancel_motion_render_tasks(&mut self) {
        self.motion_render_generation = self.motion_render_generation.wrapping_add(1).max(1);
        self.release_motion_target_reservations();
        let released = self
            .motion_render_queued
            .values()
            .map(VecDeque::len)
            .sum::<usize>() as u64;
        self.motion_render_queued.clear();
        self.motion_reservations_released =
            self.motion_reservations_released.saturating_add(released);
        if !self.motion_render_tasks.is_empty() {
            self.render_generation = self.render_generation.wrapping_add(1).max(1);
            for task in self.motion_render_tasks.values() {
                _ = task
                    .renderer
                    .supersede_page_render(&task.prepared, self.render_generation);
            }
            self.motion_render_superseded = self
                .motion_render_superseded
                .saturating_add(self.motion_render_tasks.len() as u64);
            self.motion_render_tasks.clear();
        }
    }

    fn release_motion_target_reservations(&mut self) {
        let released = self.motion_target_reservations.len() as u64;
        self.motion_target_reservations.clear();
        self.motion_reservations_released =
            self.motion_reservations_released.saturating_add(released);
    }

    fn begin_motion(&mut self) {
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

    fn begin_zoom_motion(&mut self) {
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

    fn cancel_destination_task(&mut self) {
        if let Some(cancellation) = self.destination_cancellation.take() {
            cancellation.cancel();
        }
        self.destination_task = None;
        self.destination_generation = self.destination_generation.wrapping_add(1).max(1);
    }

    fn is_active(&self) -> bool {
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
            || !self.motion_render_tasks.is_empty()
            || !self.motion_render_queued.is_empty()
            || !self.motion_target_reservations.is_empty()
            || self.speculative_resume_task.is_some()
            || !self.surface_render_pending.is_empty()
            || !self.preparation_demands.is_empty()
            || !self.terminal_pages.is_empty()
            || !self.pages.is_empty()
            || !self.rendered.is_empty()
            || !self.continuity_surfaces.is_empty()
    }

    fn mark_page_used(&mut self, page: u32) {
        self.page_usage_clock = self.page_usage_clock.wrapping_add(1).max(1);
        self.page_last_used.insert(page, self.page_usage_clock);
    }

    fn record_surface_latency(&mut self, queue: Duration, ready: Duration) {
        push_surface_latency(&mut self.surface_queue_latency, queue);
        push_surface_latency(&mut self.surface_ready_latency, ready);
    }

    fn record_surface_present_latency(&mut self, present: Duration) {
        push_surface_latency(&mut self.surface_present_latency, present);
    }

    fn prune(&mut self, visible: &HashSet<u32>, retained: &HashSet<u32>) {
        self.visible_pages.clone_from(visible);
        self.presented_pages.clear();
        self.presented_continuity_pages.clear();
        self.covered_pages.clear();
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

    fn prune_states(
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

    fn surface_bytes(&self) -> u64 {
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

    fn prepared_resource_bytes(&self) -> u64 {
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

    fn prune_prepared_resource_cache(&mut self, visible: &HashSet<u32>) {
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

    fn prune_surface_cache(&mut self) {
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

    fn reserve_surface(&mut self, page: u32, bytes: u64) -> Result<(), u64> {
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

    fn reserve_continuity_surface(&mut self, page: u32, bytes: u64) -> bool {
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

    fn active_preparations(&self) -> usize {
        self.pages
            .values()
            .filter(|state| matches!(state, GpuPageState::Preparing { .. }))
            .count()
    }

    fn active_speculative_preparations(&self) -> usize {
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

    fn worst_speculative_preparation(&self) -> Option<u32> {
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

    fn queued_preparations(&self) -> usize {
        self.preparation_demands
            .keys()
            .filter(|page| !self.pages.contains_key(page))
            .count()
    }

    fn queued_speculative_preparations(&self) -> usize {
        self.preparation_demands
            .iter()
            .filter(|(page, demand)| demand.speculative && !self.pages.contains_key(page))
            .count()
    }

    fn set_preparation_demands(&mut self, targets: &[GpuPreparationTarget]) {
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

    fn demand_page(&mut self, page: u32, speculative: bool, priority: usize) {
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

    fn cancel_active_preparation(&mut self, page: u32, retain_demand: bool) {
        if let Some(GpuPageState::Preparing { cancellation, .. }) = self.pages.remove(&page) {
            cancellation.cancel();
            self.preparation_cancelled = self.preparation_cancelled.saturating_add(1);
        }
        if !retain_demand {
            self.preparation_demands.remove(&page);
        }
    }

    pub(super) fn invalidate_link_mappings(&mut self) {
        self.link_mappings.clear();
        self.application_fallbacks.clear();
    }
}

impl Drop for GpuPdfState {
    fn drop(&mut self) {
        self.cancel_page_tasks();
    }
}

fn gpu_preparation_targets(
    visible: &HashSet<u32>,
    target_viewport: &HashSet<u32>,
    motion_history: &[u32],
    page_count: u32,
    forward: bool,
) -> (Vec<GpuPreparationTarget>, Option<u32>) {
    let Some(first) = visible.iter().min().copied() else {
        return (Vec::new(), None);
    };
    let last = visible.iter().max().copied().unwrap_or(first);
    let center = first.saturating_add(last).saturating_div(2);
    let mut visible_pages = visible.iter().copied().collect::<Vec<_>>();
    visible_pages.sort_unstable_by_key(|page| (page.abs_diff(center), *page));

    let mut ordered = visible_pages
        .into_iter()
        .enumerate()
        .map(|(priority, page)| GpuPreparationTarget {
            page,
            priority,
            speculative: false,
        })
        .collect::<Vec<_>>();
    let target_center = target_viewport
        .iter()
        .min()
        .copied()
        .zip(target_viewport.iter().max().copied())
        .map(|(first, last)| first.saturating_add(last).saturating_div(2))
        .unwrap_or(center);
    let mut target_pages = target_viewport
        .iter()
        .filter(|page| !visible.contains(page) && **page < page_count)
        .copied()
        .collect::<Vec<_>>();
    target_pages.sort_unstable_by_key(|page| (page.abs_diff(target_center), *page));
    target_pages.truncate(MAX_RETAINED_GPU_PAGES.saturating_sub(ordered.len()));
    let target_priority = ordered.len();
    ordered.extend(target_pages.into_iter().enumerate().map(|(offset, page)| {
        GpuPreparationTarget {
            page,
            priority: target_priority.saturating_add(offset),
            speculative: false,
        }
    }));

    // Preserve the most recently traversed pages before spending the
    // remaining cache window on directional speculation. This prevents a
    // direction reversal from immediately recompiling pages that were on
    // screen only a few frames ago.
    for page in motion_history.iter().rev().copied() {
        if ordered.len() >= MAX_RETAINED_GPU_PAGES {
            break;
        }
        if page < page_count && !ordered.iter().any(|target| target.page == page) {
            ordered.push(GpuPreparationTarget {
                page,
                priority: ordered.len(),
                speculative: true,
            });
        }
    }

    let speculative_budget = MAX_RETAINED_GPU_PAGES.saturating_sub(ordered.len());
    if speculative_budget == 0 {
        return (ordered, None);
    }

    fn push_speculative(
        pages: &mut Vec<u32>,
        visible: &HashSet<u32>,
        page_count: u32,
        page: Option<u32>,
    ) {
        if let Some(page) = page
            && page < page_count
            && !visible.contains(&page)
            && !pages.contains(&page)
        {
            pages.push(page);
        }
    }

    let desired = ordered
        .iter()
        .map(|target| target.page)
        .collect::<HashSet<_>>();
    let primary_budget = speculative_budget.saturating_mul(3).div_ceil(4);
    let secondary_budget = speculative_budget.saturating_sub(primary_budget);
    let mut speculative = Vec::with_capacity(speculative_budget);
    if forward {
        for distance in 1..page_count {
            if speculative.len() >= primary_budget {
                break;
            }
            push_speculative(
                &mut speculative,
                &desired,
                page_count,
                last.checked_add(distance),
            );
        }
        let secondary_limit = speculative.len().saturating_add(secondary_budget);
        for distance in 1..page_count {
            if speculative.len() >= secondary_limit {
                break;
            }
            push_speculative(
                &mut speculative,
                &desired,
                page_count,
                first.checked_sub(distance),
            );
        }
    } else {
        for distance in 1..page_count {
            if speculative.len() >= primary_budget {
                break;
            }
            push_speculative(
                &mut speculative,
                &desired,
                page_count,
                first.checked_sub(distance),
            );
        }
        let secondary_limit = speculative.len().saturating_add(secondary_budget);
        for distance in 1..page_count {
            if speculative.len() >= secondary_limit {
                break;
            }
            push_speculative(
                &mut speculative,
                &desired,
                page_count,
                last.checked_add(distance),
            );
        }
    }
    for distance in 1..page_count {
        if speculative.len() >= speculative_budget {
            break;
        }
        let (primary, secondary) = if forward {
            (last.checked_add(distance), first.checked_sub(distance))
        } else {
            (first.checked_sub(distance), last.checked_add(distance))
        };
        push_speculative(&mut speculative, &desired, page_count, primary);
        if speculative.len() < speculative_budget {
            push_speculative(&mut speculative, &desired, page_count, secondary);
        }
    }
    speculative.truncate(speculative_budget);
    let nearest = speculative.first().copied();
    let base_priority = ordered.len();
    ordered.extend(speculative.into_iter().enumerate().map(|(offset, page)| {
        GpuPreparationTarget {
            page,
            priority: base_priority.saturating_add(offset),
            speculative: true,
        }
    }));
    (ordered, nearest)
}

fn speculative_preparation_allowed(
    motion_active: bool,
    background_paused: bool,
    active_speculative: usize,
) -> bool {
    !background_paused && (!motion_active || active_speculative == 0)
}

fn visible_gpu_region(
    key: RenderKey,
    image_bounds: Bounds<Pixels>,
    viewport_bounds: Bounds<Pixels>,
) -> Option<GpuPageRegion> {
    let image_x0 = f64::from(image_bounds.origin.x.as_f32());
    let image_y0 = f64::from(image_bounds.origin.y.as_f32());
    let image_width = f64::from(image_bounds.size.width.as_f32());
    let image_height = f64::from(image_bounds.size.height.as_f32());
    if image_width <= 0.0 || image_height <= 0.0 {
        return None;
    }
    let viewport_x0 = f64::from(viewport_bounds.origin.x.as_f32());
    let viewport_y0 = f64::from(viewport_bounds.origin.y.as_f32());
    let viewport_width = f64::from(viewport_bounds.size.width.as_f32());
    let viewport_height = f64::from(viewport_bounds.size.height.as_f32());
    let x0 = image_x0.max(viewport_x0);
    let y0 = image_y0.max(viewport_y0);
    let x1 = (image_x0 + image_width).min(viewport_x0 + viewport_width);
    let y1 = (image_y0 + image_height).min(viewport_y0 + viewport_height);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }

    let page_width = f64::from(key.physical_width);
    let page_height = f64::from(key.physical_height);
    let physical_x0 = (((x0 - image_x0) / image_width) * page_width)
        .floor()
        .clamp(0.0, page_width) as u32;
    let physical_y0 = (((y0 - image_y0) / image_height) * page_height)
        .floor()
        .clamp(0.0, page_height) as u32;
    let physical_x1 = (((x1 - image_x0) / image_width) * page_width)
        .ceil()
        .clamp(0.0, page_width) as u32;
    let physical_y1 = (((y1 - image_y0) / image_height) * page_height)
        .ceil()
        .clamp(0.0, page_height) as u32;
    (physical_x1 > physical_x0 && physical_y1 > physical_y0).then_some(GpuPageRegion {
        x: physical_x0,
        y: physical_y0,
        width: physical_x1 - physical_x0,
        height: physical_y1 - physical_y0,
    })
}

fn gpu_surface_bounds(
    key: RenderKey,
    image_bounds: Bounds<Pixels>,
    region: GpuPageRegion,
) -> Bounds<Pixels> {
    let page_width = f64::from(key.physical_width.max(1));
    let page_height = f64::from(key.physical_height.max(1));
    let logical_width = f64::from(image_bounds.size.width.as_f32());
    let logical_height = f64::from(image_bounds.size.height.as_f32());
    Bounds::new(
        point(
            image_bounds.origin.x + px((f64::from(region.x) / page_width * logical_width) as f32),
            image_bounds.origin.y + px((f64::from(region.y) / page_height * logical_height) as f32),
        ),
        size(
            px((f64::from(region.width) / page_width * logical_width) as f32),
            px((f64::from(region.height) / page_height * logical_height) as f32),
        ),
    )
}

fn gpu_paint_texture(
    key: RenderKey,
    image_bounds: Bounds<Pixels>,
    texture: GpuPageTexture,
) -> GpuPaintTexture {
    let [source_x, source_y, source_width, source_height] = texture.source_bounds();
    GpuPaintTexture {
        bounds: gpu_surface_bounds(key, image_bounds, texture.region()),
        source_bounds: Bounds::new(point(source_x, source_y), size(source_width, source_height)),
        texture,
    }
}

fn adjacent_gpu_viewport_bounds(
    image_bounds: Bounds<Pixels>,
    viewport_bounds: Bounds<Pixels>,
    forward: bool,
) -> Bounds<Pixels> {
    let y = if forward {
        image_bounds.origin.y
    } else {
        image_bounds.origin.y + image_bounds.size.height - viewport_bounds.size.height
    };
    Bounds::new(point(viewport_bounds.origin.x, y), viewport_bounds.size)
}

impl GpuSurfaceIntent {
    const fn is_speculative(self) -> bool {
        matches!(self, Self::AdjacentWarm)
    }
}

fn should_start_gpu_fallback_grace(engine: PdfRenderEngine, intent: GpuSurfaceIntent) -> bool {
    engine == PdfRenderEngine::GlyphstFirst && intent == GpuSurfaceIntent::Visible
}

fn should_demand_missing_gpu_page(intent: GpuSurfaceIntent) -> bool {
    intent == GpuSurfaceIntent::Visible
}

fn gpu_paint_request(
    key: RenderKey,
    visible_region: GpuPageRegion,
    settled: bool,
    target_capacity: Option<[u32; 2]>,
    motion_target_capacity: Option<[u32; 2]>,
) -> GpuPaintRequest {
    GpuPaintRequest {
        page_size: [key.physical_width, key.physical_height],
        visible_region,
        target_capacity,
        motion_target_capacity,
        rotation: key.rotation,
        background: [1.0, 1.0, 1.0, 1.0],
        settled,
        // A render generation is assigned only after exact-cache and
        // scheduler coalescing checks prove this is material new work.
        generation: 0,
    }
}

fn physical_gpu_viewport_size(
    key: RenderKey,
    image_bounds: Bounds<Pixels>,
    viewport_bounds: Bounds<Pixels>,
) -> Option<[u32; 2]> {
    let image_width = image_bounds.size.width.as_f32();
    let image_height = image_bounds.size.height.as_f32();
    let viewport_width = viewport_bounds.size.width.as_f32();
    let viewport_height = viewport_bounds.size.height.as_f32();
    if image_width <= 0.0 || image_height <= 0.0 || viewport_width <= 0.0 || viewport_height <= 0.0
    {
        return None;
    }

    let physical_per_logical = [
        key.physical_width as f64 / f64::from(image_width),
        key.physical_height as f64 / f64::from(image_height),
    ];
    let viewport = [viewport_width, viewport_height].map(f64::from);
    let mut extent = [0_u32; 2];
    for axis in 0..2 {
        extent[axis] = (viewport[axis] * physical_per_logical[axis])
            .ceil()
            .clamp(1.0, f64::from(u32::MAX)) as u32;
    }
    Some(extent)
}

fn stable_gpu_target_capacity(
    key: RenderKey,
    image_bounds: Bounds<Pixels>,
    viewport_bounds: Bounds<Pixels>,
    dimension_limit: u32,
) -> Option<[u32; 2]> {
    if dimension_limit == 0 {
        return None;
    }
    let viewport = physical_gpu_viewport_size(key, image_bounds, viewport_bounds)?;
    let mut capacity = [0_u32; 2];
    for axis in 0..2 {
        let gutter = (viewport[axis] / 4).max(GPU_TARGET_ALIGNMENT);
        capacity[axis] = viewport[axis]
            .saturating_add(gutter.saturating_mul(2))
            .div_ceil(GPU_TARGET_ALIGNMENT)
            .saturating_mul(GPU_TARGET_ALIGNMENT)
            .min(dimension_limit);
    }
    Some(capacity)
}

fn stable_gpu_motion_target_capacity(
    key: RenderKey,
    image_bounds: Bounds<Pixels>,
    viewport_bounds: Bounds<Pixels>,
    dimension_limit: u32,
) -> Option<[u32; 2]> {
    if dimension_limit == 0 {
        return None;
    }
    Some(
        physical_gpu_viewport_size(key, image_bounds, viewport_bounds)?.map(|extent| {
            extent
                .div_ceil(GPU_TARGET_ALIGNMENT)
                .saturating_mul(GPU_TARGET_ALIGNMENT)
                .min(dimension_limit)
        }),
    )
}

fn should_warm_adjacent_surface(
    motion_active: bool,
    all_visible_covered: bool,
    visible_refinement_pending: bool,
    visible_patch_lod_pending: bool,
    surface_pipeline_idle: bool,
    candidate_ready: bool,
    budget_fits: bool,
) -> bool {
    all_visible_covered
        && !visible_patch_lod_pending
        && (motion_active || !visible_refinement_pending)
        && surface_pipeline_idle
        && candidate_ready
        && budget_fits
}

fn speculative_surface_fits(protected_bytes: u64, target_bytes: u64, limit: u64) -> bool {
    protected_bytes
        .checked_add(target_bytes)
        .is_some_and(|total| total <= limit)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurfaceCachePressureAction {
    PreserveDiagnostics,
    ClearDiagnostics,
    RouteToFallback,
}

fn surface_cache_pressure_action(
    speculative: bool,
    has_current: bool,
) -> SurfaceCachePressureAction {
    if has_current {
        SurfaceCachePressureAction::ClearDiagnostics
    } else if speculative {
        SurfaceCachePressureAction::PreserveDiagnostics
    } else {
        SurfaceCachePressureAction::RouteToFallback
    }
}

fn prewarmed_surface_covers(
    prewarmed: bool,
    rendered_key: RenderKey,
    rendered_region: GpuPageRegion,
    requested_key: RenderKey,
    visible_region: GpuPageRegion,
    same_device: bool,
) -> bool {
    prewarmed
        && same_device
        && motion_surface_density_sufficient(rendered_key, requested_key)
        && gpu_region_contains(rendered_region, visible_region)
}

fn should_start_surface_seed(settled: bool, allow_motion_seed: bool, has_previous: bool) -> bool {
    settled || allow_motion_seed || !has_previous
}

fn should_defer_full_surface_render(
    settled: bool,
    allow_motion_seed: bool,
    has_previous: bool,
) -> bool {
    !should_start_surface_seed(settled, allow_motion_seed, has_previous)
}

fn assign_surface_render_generation(counter: &mut u64, request: &mut GpuPaintRequest) {
    *counter = counter.wrapping_add(1).max(1);
    request.generation = *counter;
}

fn same_gpu_surface_content(left: GpuPaintRequest, right: GpuPaintRequest) -> bool {
    // The visible region, motion quality, and request generation choose work;
    // they do not change the identity of already-rendered page pixels.
    left.page_size == right.page_size
        && left.target_capacity == right.target_capacity
        && left.motion_target_capacity == right.motion_target_capacity
        && left.rotation == right.rotation
        && left.background == right.background
}

fn same_gpu_surface_work(left: GpuPaintRequest, right: GpuPaintRequest) -> bool {
    same_gpu_surface_content(left, right) && left.settled == right.settled
}

fn same_gpu_page_source(left: RenderKey, right: RenderKey) -> bool {
    left.doc_id == right.doc_id
        && left.page_idx == right.page_idx
        && left.source_revision == right.source_revision
        && left.rotation == right.rotation
        && left.color_transform == right.color_transform
}

fn same_gpu_page_dimensions(left: RenderKey, right: RenderKey) -> bool {
    left.physical_width == right.physical_width && left.physical_height == right.physical_height
}

fn continuity_render_key(exact: RenderKey) -> RenderKey {
    let width = u64::from(exact.physical_width.max(1));
    let height = u64::from(exact.physical_height.max(1));
    let longest = width.max(height);
    if longest <= u64::from(GPU_CONTINUITY_LONG_EDGE) {
        return exact;
    }
    let scaled = |dimension: u64| {
        dimension
            .saturating_mul(u64::from(GPU_CONTINUITY_LONG_EDGE))
            .div_ceil(longest)
            .clamp(1, u64::from(GPU_CONTINUITY_LONG_EDGE)) as u32
    };
    RenderKey {
        physical_width: scaled(width),
        physical_height: scaled(height),
        ..exact
    }
}

fn full_page_gpu_region(key: RenderKey) -> GpuPageRegion {
    GpuPageRegion {
        x: 0,
        y: 0,
        width: key.physical_width,
        height: key.physical_height,
    }
}

fn motion_surface_density_sufficient(rendered: RenderKey, requested: RenderKey) -> bool {
    same_gpu_page_source(rendered, requested)
        && u64::from(rendered.physical_width).saturating_mul(GPU_MOTION_MIN_DENSITY_DENOMINATOR)
            >= u64::from(requested.physical_width).saturating_mul(GPU_MOTION_MIN_DENSITY_NUMERATOR)
        && u64::from(rendered.physical_height).saturating_mul(GPU_MOTION_MIN_DENSITY_DENOMINATOR)
            >= u64::from(requested.physical_height).saturating_mul(GPU_MOTION_MIN_DENSITY_NUMERATOR)
}

fn gpu_region_contains(outer: GpuPageRegion, inner: GpuPageRegion) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

fn gpu_regions_overlap(left: GpuPageRegion, right: GpuPageRegion) -> bool {
    left.x < right.right()
        && right.x < left.right()
        && left.y < right.bottom()
        && right.y < left.bottom()
}

fn presented_surface_kind(
    primary_covers: bool,
    continuity_covers: bool,
    primary_overlaps: bool,
) -> Option<PresentedSurfaceKind> {
    if primary_covers {
        Some(PresentedSurfaceKind::Primary)
    } else if continuity_covers {
        Some(PresentedSurfaceKind::Continuity)
    } else if primary_overlaps {
        Some(PresentedSurfaceKind::PartialPrimary)
    } else {
        None
    }
}

fn should_defer_fixed_scale_scroll_refresh(
    settled: bool,
    current_dimensions_exact: bool,
    primary_covers: bool,
    continuity_covers: bool,
) -> bool {
    !settled && current_dimensions_exact && primary_covers && continuity_covers
}

fn should_present_continuity_motion(
    settled: bool,
    current_dimensions_exact: bool,
    zoom_motion_active: bool,
    selected: Option<PresentedSurfaceKind>,
) -> bool {
    !settled
        && (!zoom_motion_active || current_dimensions_exact)
        && selected == Some(PresentedSurfaceKind::Continuity)
}

fn zoom_motion_cancels_surface_work(
    has_coherent_surface: bool,
    role: SurfaceRenderRole,
    speculative: bool,
) -> bool {
    speculative || role == SurfaceRenderRole::Continuity || has_coherent_surface
}

/// Keeps the original visible region in the middle half of every available
/// overscan margin. Crossing this guard starts the next surface while the
/// current texture still covers the viewport.
fn surface_refresh_guard(surface: GpuPageRegion, seed_visible: GpuPageRegion) -> GpuPageRegion {
    if !gpu_region_contains(surface, seed_visible) {
        return surface;
    }

    let left_margin = seed_visible.x.saturating_sub(surface.x);
    let top_margin = seed_visible.y.saturating_sub(surface.y);
    let right_margin = surface.right().saturating_sub(seed_visible.right());
    let bottom_margin = surface.bottom().saturating_sub(seed_visible.bottom());
    let x = surface.x.saturating_add(left_margin / 2);
    let y = surface.y.saturating_add(top_margin / 2);
    let right = surface.right().saturating_sub(right_margin / 2);
    let bottom = surface.bottom().saturating_sub(bottom_margin / 2);
    GpuPageRegion {
        x,
        y,
        width: right.saturating_sub(x),
        height: bottom.saturating_sub(y),
    }
}

fn surface_request_covers(
    existing: SurfaceRequestIdentity,
    incoming: SurfaceRequestIdentity,
) -> bool {
    existing.key == incoming.key
        && same_gpu_surface_work(existing.request, incoming.request)
        && gpu_region_contains(existing.region, incoming.request.visible_region)
}

fn can_coalesce_pending_surface_request(
    pending: SurfaceRequestIdentity,
    pending_speculative: bool,
    incoming: SurfaceRequestIdentity,
    incoming_speculative: bool,
) -> bool {
    surface_request_covers(pending, incoming) && (!pending_speculative || incoming_speculative)
}

fn can_reuse_active_surface_request(
    same_request: bool,
    active_speculative: bool,
    incoming_speculative: bool,
) -> bool {
    same_request && (!active_speculative || incoming_speculative)
}

fn push_surface_latency(samples: &mut VecDeque<Duration>, sample: Duration) {
    if samples.len() == MAX_SURFACE_LATENCY_SAMPLES {
        samples.pop_front();
    }
    samples.push_back(sample);
}

fn surface_latency_p95(samples: &VecDeque<Duration>) -> Duration {
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

fn pending_surface_priority(
    page: u32,
    role: SurfaceRenderRole,
    speculative: bool,
    sequence: u64,
) -> (u8, u64, u32) {
    let priority = match (role, speculative) {
        (SurfaceRenderRole::Primary, false) => 0,
        (SurfaceRenderRole::Continuity, false) => 1,
        (SurfaceRenderRole::Primary, true) => 2,
        (SurfaceRenderRole::Continuity, true) => 3,
    };
    (priority, sequence, page)
}

fn mupdf_placeholder_allowed(
    engine: PdfRenderEngine,
    terminal_native_route: bool,
    grace_elapsed: bool,
) -> bool {
    match engine {
        PdfRenderEngine::MuPdf => true,
        PdfRenderEngine::Glyphst => false,
        PdfRenderEngine::GlyphstFirst => terminal_native_route || grace_elapsed,
    }
}

impl DocumentView {
    pub(super) fn strict_native_pdf(&self, cx: &gpui::App) -> bool {
        self.format == DocumentFormat::Pdf
            && cx
                .global::<SettingsStore>()
                .settings
                .document_view
                .pdf_render_engine
                .is_strict_native()
    }

    pub(super) fn native_gpu_document(&self) -> Option<Arc<GpuPdfDocument>> {
        self.gpu_pdf.document.clone()
    }

    fn start_native_gpu_fallback_grace(&mut self, page: u32, cx: &mut Context<Self>) {
        if self.gpu_pdf.fallback_grace_elapsed.contains(&page)
            || self.gpu_pdf.fallback_grace_tasks.contains_key(&page)
        {
            return;
        }
        let document_generation = self.gpu_pdf.generation;
        self.gpu_pdf.fallback_grace_generation = self
            .gpu_pdf
            .fallback_grace_generation
            .wrapping_add(1)
            .max(1);
        let generation = self.gpu_pdf.fallback_grace_generation;
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(GPU_FIRST_PAINT_GRACE).await;
            let _ = this.update(cx, |this, cx| {
                if this.gpu_pdf.generation != document_generation
                    || !matches!(
                        this.gpu_pdf.fallback_grace_tasks.get(&page),
                        Some(task) if task.generation == generation
                    )
                {
                    return;
                }
                this.gpu_pdf.fallback_grace_tasks.remove(&page);
                this.gpu_pdf.fallback_grace_elapsed.insert(page);
                cx.notify();
            });
        });
        self.gpu_pdf.fallback_grace_tasks.insert(
            page,
            FallbackGraceTask {
                generation,
                _task: task,
            },
        );
    }

    pub(super) fn native_gpu_mupdf_placeholder_allowed(
        &self,
        page: u32,
        engine: PdfRenderEngine,
    ) -> bool {
        if self.format != DocumentFormat::Pdf {
            return true;
        }
        let terminal_native_route = self.gpu_pdf.document_error.is_some()
            || self.gpu_pdf.renderer_error.is_some()
            || self.gpu_pdf.surface_failures.contains_key(&page)
            || self.gpu_pdf.terminal_pages.contains(&page)
            || matches!(
                self.gpu_pdf.pages.get(&page),
                Some(GpuPageState::Fallback(_) | GpuPageState::Failed(_))
            );
        mupdf_placeholder_allowed(
            engine,
            terminal_native_route,
            self.gpu_pdf.fallback_grace_elapsed.contains(&page),
        )
    }

    pub(super) fn mark_native_gpu_motion(&mut self, cx: &mut Context<Self>) {
        self.gpu_pdf.begin_motion();
        if self.gpu_pdf.speculative_resume_task.take().is_some() {
            self.gpu_pdf.speculative_resume_generation = self
                .gpu_pdf
                .speculative_resume_generation
                .wrapping_add(1)
                .max(1);
        }
        self.gpu_pdf.motion_active = true;
        self.gpu_pdf.motion_deadline = Some(Instant::now() + GPU_MOTION_SETTLE_DELAY);
        if self.gpu_pdf.motion_settle_task.is_some() {
            return;
        }
        self.gpu_pdf.motion_generation = self.gpu_pdf.motion_generation.wrapping_add(1).max(1);
        let generation = self.gpu_pdf.motion_generation;
        self.gpu_pdf.motion_settle_task = Some(cx.spawn(async move |this, cx| {
            let mut remaining = GPU_MOTION_SETTLE_DELAY;
            loop {
                cx.background_executor().timer(remaining).await;
                let Ok(next) = this.update(cx, |this, cx| {
                    if this.gpu_pdf.motion_generation != generation {
                        return None;
                    }
                    let now = Instant::now();
                    if let Some(deadline) = this.gpu_pdf.motion_deadline
                        && now < deadline
                    {
                        return Some(deadline.duration_since(now));
                    }
                    this.gpu_pdf.motion_active = false;
                    this.gpu_pdf.zoom_motion_active = false;
                    this.gpu_pdf.release_motion_target_reservations();
                    this.gpu_pdf.motion_deadline = None;
                    this.gpu_pdf.motion_settle_task = None;
                    this.gpu_pdf.motion_retained_pages.clear();
                    this.schedule_native_gpu_speculative_resume(cx);
                    this.dispatch_native_gpu_page_preparations(cx);
                    this.dispatch_native_gpu_surface_render(cx);
                    cx.notify();
                    None
                }) else {
                    break;
                };
                let Some(next) = next else {
                    break;
                };
                remaining = next;
            }
        }));
    }

    fn schedule_native_gpu_speculative_resume(&mut self, cx: &mut Context<Self>) {
        self.gpu_pdf.speculative_resume_generation = self
            .gpu_pdf
            .speculative_resume_generation
            .wrapping_add(1)
            .max(1);
        let generation = self.gpu_pdf.speculative_resume_generation;
        let document_generation = self.gpu_pdf.generation;
        self.gpu_pdf.speculative_resume_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(GPU_SPECULATIVE_RESUME_DELAY)
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.gpu_pdf.generation != document_generation
                    || this.gpu_pdf.speculative_resume_generation != generation
                {
                    return;
                }
                this.gpu_pdf.speculative_resume_task = None;
                this.dispatch_native_gpu_page_preparations(cx);
                this.dispatch_native_gpu_surface_render(cx);
                cx.notify();
            });
        }));
    }

    pub(super) fn mark_native_gpu_zoom_motion(&mut self, cx: &mut Context<Self>) {
        self.gpu_pdf.begin_zoom_motion();
        self.mark_native_gpu_motion(cx);
    }

    pub(super) fn native_gpu_zoom_frame_ready(&mut self) -> bool {
        if !self.gpu_pdf.zoom_motion_active {
            return true;
        }

        if !self.gpu_pdf.surface_seed_tasks.is_empty() {
            self.gpu_pdf.surface_zoom_worker_holds =
                self.gpu_pdf.surface_zoom_worker_holds.saturating_add(1);
            self.gpu_pdf.surface_zoom_admission_holds =
                self.gpu_pdf.surface_zoom_admission_holds.saturating_add(1);
            self.gpu_pdf.surface_zoom_gpu_holds =
                self.gpu_pdf.surface_zoom_gpu_holds.saturating_add(1);
            return false;
        }

        let mut pages = self
            .gpu_pdf
            .presented_pages
            .iter()
            .filter(|page| self.gpu_pdf.rendered.contains_key(page))
            .copied()
            .collect::<Vec<_>>();
        pages.sort_unstable();
        if pages.is_empty() {
            self.gpu_pdf.release_motion_target_reservations();
            return true;
        }

        // Every admitted scale step needs a writable retained-render target.
        // Raster density envelopes are intentionally not an alternate source.
        let pages_requiring_targets = pages;
        let reservations_match = self.gpu_pdf.motion_target_reservations.len()
            == pages_requiring_targets.len()
            && pages_requiring_targets
                .iter()
                .all(|page| self.gpu_pdf.motion_target_reservations.contains_key(page));
        if reservations_match {
            return true;
        }
        self.gpu_pdf.release_motion_target_reservations();

        let Some(renderer) = self.gpu_pdf.renderer.clone() else {
            return true;
        };
        let mut reservations = HashMap::with_capacity(pages_requiring_targets.len());
        for page in pages_requiring_targets {
            let Some(rendered) = self.gpu_pdf.rendered.get(&page) else {
                continue;
            };
            match renderer.reserve_motion_target(&rendered.presentation) {
                Ok(Some(reservation)) => {
                    reservations.insert(page, reservation);
                    self.gpu_pdf.motion_reservations_started =
                        self.gpu_pdf.motion_reservations_started.saturating_add(1);
                }
                Ok(None) => {
                    self.gpu_pdf.motion_reservations_released = self
                        .gpu_pdf
                        .motion_reservations_released
                        .saturating_add(reservations.len() as u64);
                    drop(reservations);
                    self.gpu_pdf.surface_zoom_sample_holds =
                        self.gpu_pdf.surface_zoom_sample_holds.saturating_add(1);
                    self.gpu_pdf.surface_zoom_admission_holds =
                        self.gpu_pdf.surface_zoom_admission_holds.saturating_add(1);
                    self.gpu_pdf.surface_zoom_gpu_holds =
                        self.gpu_pdf.surface_zoom_gpu_holds.saturating_add(1);
                    return false;
                }
                Err(error) => {
                    self.gpu_pdf.motion_reservations_released = self
                        .gpu_pdf
                        .motion_reservations_released
                        .saturating_add(reservations.len() as u64);
                    drop(reservations);
                    if renderer.is_device_lost() {
                        self.gpu_pdf.reset_device();
                    } else {
                        eprintln!("failed to reserve native PDF zoom target: {error:#}");
                    }
                    self.gpu_pdf.surface_zoom_sample_holds =
                        self.gpu_pdf.surface_zoom_sample_holds.saturating_add(1);
                    self.gpu_pdf.surface_zoom_admission_holds =
                        self.gpu_pdf.surface_zoom_admission_holds.saturating_add(1);
                    self.gpu_pdf.surface_zoom_gpu_holds =
                        self.gpu_pdf.surface_zoom_gpu_holds.saturating_add(1);
                    return false;
                }
            }
        }
        self.gpu_pdf.motion_target_reservations = reservations;
        true
    }

    pub(super) fn record_native_gpu_zoom_step(&mut self) {
        if self.gpu_pdf.renderer.is_some() && self.gpu_pdf.zoom_motion_active {
            self.gpu_pdf.surface_zoom_animation_steps =
                self.gpu_pdf.surface_zoom_animation_steps.saturating_add(1);
        }
    }

    pub(super) fn native_gpu_key_enabled(&self, key: RenderKey, engine: PdfRenderEngine) -> bool {
        engine.uses_native_reader()
            && self.format == DocumentFormat::Pdf
            && key.source_revision == 0
            && key.color_transform.is_none()
    }

    fn selected_gpu_surface(
        &self,
        page: u32,
        requested_key: RenderKey,
        image_bounds: Bounds<Pixels>,
        viewport_bounds: Bounds<Pixels>,
        renderer_token: ExternalGpuDeviceToken,
        current: Option<&CurrentGpuSurface>,
    ) -> (Option<SelectedGpuSurface>, bool) {
        let primary_covers = current.is_some_and(|current| {
            gpu_region_contains(current.texture.region(), current.visible_region)
        });
        let primary_overlaps = current.is_some_and(|current| {
            gpu_regions_overlap(current.texture.region(), current.visible_region)
        });
        let continuity = self
            .gpu_pdf
            .continuity_surfaces
            .get(&page)
            .and_then(|surface| {
                if !same_gpu_page_source(surface.key, requested_key)
                    || surface.texture.token() != renderer_token
                {
                    return None;
                }
                let visible_region =
                    visible_gpu_region(surface.key, image_bounds, viewport_bounds)?;
                gpu_region_contains(surface.texture.region(), visible_region).then_some(surface)
            });
        let continuity_covers = continuity.is_some();
        let Some(kind) =
            presented_surface_kind(primary_covers, continuity_covers, primary_overlaps)
        else {
            return (None, continuity_covers);
        };
        let selected = match kind {
            PresentedSurfaceKind::Primary | PresentedSurfaceKind::PartialPrimary => {
                let current = current.expect("primary presentation was selected");
                Some(SelectedGpuSurface {
                    key: current.key,
                    texture: current.texture.clone(),
                    kind,
                })
            }
            PresentedSurfaceKind::Continuity => {
                let continuity = continuity.expect("continuity presentation was selected");
                Some(SelectedGpuSurface {
                    key: continuity.key,
                    texture: continuity.texture.clone(),
                    kind,
                })
            }
        };
        (selected, continuity_covers)
    }

    fn present_gpu_surface(
        &mut self,
        page: u32,
        image_bounds: Bounds<Pixels>,
        selected: SelectedGpuSurface,
    ) -> GpuPaintTexture {
        let enqueued_at = match selected.kind {
            PresentedSurfaceKind::Primary | PresentedSurfaceKind::PartialPrimary => {
                self.gpu_pdf.rendered.get_mut(&page).and_then(|rendered| {
                    if rendered.first_paint_recorded {
                        None
                    } else {
                        rendered.first_paint_recorded = true;
                        Some(rendered.enqueued_at)
                    }
                })
            }
            PresentedSurfaceKind::Continuity => self
                .gpu_pdf
                .continuity_surfaces
                .get_mut(&page)
                .and_then(|continuity| {
                    let first =
                        (!continuity.first_paint_recorded).then_some(continuity.enqueued_at);
                    continuity.first_paint_recorded = true;
                    if continuity.prewarmed {
                        continuity.prewarmed = false;
                        self.gpu_pdf.surface_warm_hits =
                            self.gpu_pdf.surface_warm_hits.saturating_add(1);
                    }
                    first
                }),
        };
        match selected.kind {
            PresentedSurfaceKind::Primary => {
                self.gpu_pdf.covered_pages.insert(page);
                self.gpu_pdf.presented_continuity_pages.remove(&page);
            }
            PresentedSurfaceKind::Continuity => {
                self.gpu_pdf.covered_pages.insert(page);
                self.gpu_pdf.presented_continuity_pages.insert(page);
                self.gpu_pdf.surface_continuity_hits =
                    self.gpu_pdf.surface_continuity_hits.saturating_add(1);
            }
            PresentedSurfaceKind::PartialPrimary => {
                self.gpu_pdf.presented_continuity_pages.remove(&page);
                self.gpu_pdf.surface_partial_reuses =
                    self.gpu_pdf.surface_partial_reuses.saturating_add(1);
            }
        }
        self.gpu_pdf.presented_pages.insert(page);
        self.gpu_pdf.surface_failures.remove(&page);
        if let Some(enqueued_at) = enqueued_at {
            self.gpu_pdf.record_surface_present_latency(
                Instant::now().saturating_duration_since(enqueued_at),
            );
        }
        let paint = gpu_paint_texture(selected.key, image_bounds, selected.texture);
        if matches!(
            selected.kind,
            PresentedSurfaceKind::Primary | PresentedSurfaceKind::PartialPrimary
        ) && let Some(rendered) = self.gpu_pdf.rendered.get_mut(&page)
        {
            rendered.last_paint = Some(paint.clone());
        }
        paint
    }

    fn present_selected_or_record_gap(
        &mut self,
        page: u32,
        image_bounds: Bounds<Pixels>,
        selected: Option<SelectedGpuSurface>,
    ) -> Option<GpuPaintTexture> {
        if let Some(selected) = selected {
            return Some(self.present_gpu_surface(page, image_bounds, selected));
        }
        if self.gpu_pdf.rendered.contains_key(&page)
            || self.gpu_pdf.continuity_surfaces.contains_key(&page)
        {
            self.gpu_pdf.surface_presentation_gaps =
                self.gpu_pdf.surface_presentation_gaps.saturating_add(1);
        }
        None
    }

    fn hold_last_sharp_gpu_texture(&mut self, page: u32) -> Option<GpuPaintTexture> {
        let held = self
            .gpu_pdf
            .rendered
            .get(&page)
            .and_then(|rendered| rendered.last_paint.clone());
        if held.is_some() {
            // Presentation bookkeeping is rebuilt on every paint. A detached
            // motion render deliberately keeps the last completed GPU texture
            // visible, so record that texture as the presented backend too.
            // Otherwise diagnostics (and link activation) briefly classify a
            // genuinely GPU-painted frame as `glyphst-pending` while the next
            // sharp target is in flight.
            self.gpu_pdf.presented_pages.insert(page);
            self.gpu_pdf.presented_continuity_pages.remove(&page);
            self.gpu_pdf.surface_failures.remove(&page);
            self.gpu_pdf.surface_zoom_presentation_holds = self
                .gpu_pdf
                .surface_zoom_presentation_holds
                .saturating_add(1);
        }
        held
    }

    fn start_native_gpu_zoom_frame(
        &mut self,
        page: u32,
        key: RenderKey,
        prepared: Arc<PreparedGpuPage>,
        renderer: Arc<GpuPdfRenderer>,
        request: GpuPaintRequest,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        let presentation = self
            .gpu_pdf
            .rendered
            .get(&page)
            .map(|rendered| rendered.presentation.clone())
            .ok_or_else(|| {
                "sharp native PDF zoom redraw has no retained presentation".to_string()
            })?;
        let previous_surface_bytes = presentation.byte_len();
        let Some(reservation) = self.gpu_pdf.motion_target_reservations.remove(&page) else {
            return Ok(false);
        };
        self.gpu_pdf.motion_render_generation =
            self.gpu_pdf.motion_render_generation.wrapping_add(1).max(1);
        let generation = self.gpu_pdf.motion_render_generation;
        let queued_frame = QueuedMotionRender {
            page,
            generation,
            key,
            request,
            presentation,
            previous_surface_bytes,
            prepared,
            renderer,
            reservation,
            enqueued_at: Instant::now(),
        };
        if self.gpu_pdf.motion_render_tasks.contains_key(&page) {
            let queued = self.gpu_pdf.motion_render_queued.entry(page).or_default();
            let replaced = replace_queued_motion(queued, queued_frame);
            self.gpu_pdf.motion_reservations_released = self
                .gpu_pdf
                .motion_reservations_released
                .saturating_add(replaced as u64);
            self.gpu_pdf.motion_render_superseded = self
                .gpu_pdf
                .motion_render_superseded
                .saturating_add(replaced as u64);
            self.gpu_pdf.motion_render_peak = self
                .gpu_pdf
                .motion_render_peak
                .max(queued.len().saturating_add(1));
            return Ok(true);
        }
        self.launch_native_gpu_zoom_frame(queued_frame, cx)?;
        Ok(true)
    }

    fn launch_native_gpu_zoom_frame(
        &mut self,
        queued: QueuedMotionRender,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let QueuedMotionRender {
            page,
            generation,
            key,
            mut request,
            mut presentation,
            previous_surface_bytes,
            prepared,
            renderer,
            reservation,
            enqueued_at,
        } = queued;
        debug_assert!(!self.gpu_pdf.motion_render_tasks.contains_key(&page));
        assign_surface_render_generation(&mut self.gpu_pdf.render_generation, &mut request);
        if let Err(error) = renderer.supersede_page_render(&prepared, request.generation) {
            self.gpu_pdf.motion_reservations_released =
                self.gpu_pdf.motion_reservations_released.saturating_add(1);
            return Err(format!("{error:#}"));
        }
        self.gpu_pdf.motion_reservations_consumed =
            self.gpu_pdf.motion_reservations_consumed.saturating_add(1);
        let document_generation = self.gpu_pdf.generation;
        let token = renderer.token();
        let worker_page = Arc::clone(&prepared);
        let worker_renderer = Arc::clone(&renderer);
        let completion_page = Arc::clone(&prepared);
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let queue_wait = enqueued_at.elapsed();
                    let execution_started = Instant::now();
                    worker_renderer
                        .render_motion_page(
                            &worker_page,
                            request,
                            reservation,
                            &mut presentation,
                        )
                        .map(|outcome| {
                            (
                                presentation,
                                outcome,
                                queue_wait,
                                execution_started.elapsed(),
                            )
                        })
                        .map_err(|error| format!("{error:#}"))
                })
                .await;
            let update_result = this.update(cx, |this, cx| {
                if this.gpu_pdf.generation != document_generation
                    || !matches!(
                        this.gpu_pdf.motion_render_tasks.get(&page),
                        Some(task) if task.page == page && task.generation == generation
                    )
                {
                    return;
                }
                let Some(completed_task) = this.gpu_pdf.motion_render_tasks.remove(&page) else {
                    return;
                };
                this.gpu_pdf.motion_render_completed =
                    this.gpu_pdf.motion_render_completed.saturating_add(1);
                if this
                    .gpu_pdf
                    .renderer
                    .as_ref()
                    .is_none_or(|current| current.token() != token)
                    || !matches!(
                        this.gpu_pdf.pages.get(&page),
                        Some(GpuPageState::Ready(current))
                            if Arc::ptr_eq(current, &completion_page)
                    )
                {
                    this.gpu_pdf.motion_render_superseded =
                        this.gpu_pdf.motion_render_superseded.saturating_add(1);
                    if let Some(queued) = this.gpu_pdf.motion_render_queued.remove(&page) {
                        this.gpu_pdf.motion_reservations_released = this
                            .gpu_pdf
                            .motion_reservations_released
                            .saturating_add(queued.len() as u64);
                    }
                    cx.notify();
                    return;
                }
                match result {
                    Ok((
                        presentation,
                        GpuPageRenderOutcome::Rendered {
                            presented: Some(texture),
                            stats,
                        },
                        queue_wait,
                        execution,
                    )) => {
                        let elapsed = queue_wait.saturating_add(execution);
                        if elapsed >= Duration::from_millis(8) {
                            eprintln!(
                                "slow detached native PDF zoom redraw on page {}: total {:.3} ms, worker queue {:.3} ms, execution {:.3} ms, renderer CPU {:.3} ms, encoding {:.3} ms, pool wait {:.3} ms, pool grew {}, transient reuse {}/{}, transient {:.1}/{:.1} MiB, LOD {}, target {}x{}",
                                page + 1,
                                elapsed.as_secs_f64() * 1_000.0,
                                queue_wait.as_secs_f64() * 1_000.0,
                                execution.as_secs_f64() * 1_000.0,
                                stats.cpu_time.as_secs_f64() * 1_000.0,
                                stats.command_encoding_time.as_secs_f64() * 1_000.0,
                                stats.frame_pool_wait_time.as_secs_f64() * 1_000.0,
                                stats.frame_pool_grew,
                                stats.transient_pool_reuses,
                                stats.offscreen_passes,
                                stats.transient_peak_bytes as f64 / (1024.0 * 1024.0),
                                stats.transient_resident_bytes as f64 / (1024.0 * 1024.0),
                                stats.selected_vector_lod,
                                request.motion_target_capacity.map_or(
                                    request.visible_region.width,
                                    |size| size[0],
                                ),
                                request.motion_target_capacity.map_or(
                                    request.visible_region.height,
                                    |size| size[1],
                                ),
                            );
                        }
                        let bytes = presentation.byte_len();
                        if let Err(required) = this.gpu_pdf.reserve_surface(page, bytes) {
                            this.gpu_pdf.surface_zoom_live_failures =
                                this.gpu_pdf.surface_zoom_live_failures.saturating_add(1);
                            eprintln!(
                                "discarding sharp native PDF zoom redraw on page {}: surfaces require {required} bytes, exceeding the {MAX_RETAINED_GPU_SURFACE_BYTES}-byte cache limit",
                                page + 1,
                            );
                        } else if let Some(rendered) = this.gpu_pdf.rendered.get_mut(&page) {
                            rendered.key = key;
                            rendered.request = request;
                            rendered.refresh_region =
                                surface_refresh_guard(texture.region(), request.visible_region);
                            rendered.presentation = presentation;
                            rendered.latest_stats = stats;
                            rendered.prewarmed = false;
                            this.gpu_pdf.surface_zoom_live_redraws =
                                this.gpu_pdf.surface_zoom_live_redraws.saturating_add(1);
                            this.gpu_pdf.surface_zoom_target_reuses =
                                this.gpu_pdf.surface_zoom_target_reuses.saturating_add(1);
                            this.gpu_pdf.motion_render_detached_reuses = this
                                .gpu_pdf
                                .motion_render_detached_reuses
                                .saturating_add(1);
                            debug_assert!(bytes <= previous_surface_bytes);
                            this.gpu_pdf.surface_failures.remove(&page);
                        } else {
                            this.gpu_pdf.motion_render_superseded =
                                this.gpu_pdf.motion_render_superseded.saturating_add(1);
                        }
                    }
                    Ok((_, GpuPageRenderOutcome::Superseded, _, _)) => {
                        this.gpu_pdf.motion_render_superseded =
                            this.gpu_pdf.motion_render_superseded.saturating_add(1);
                    }
                    Ok((
                        _,
                        GpuPageRenderOutcome::Rendered { presented: None, .. },
                        _,
                        _,
                    ))
                    | Ok((_, GpuPageRenderOutcome::Deferred, _, _)) => {
                        this.gpu_pdf.surface_zoom_live_failures =
                            this.gpu_pdf.surface_zoom_live_failures.saturating_add(1);
                    }
                    Err(error) => {
                        if completed_task.renderer.is_device_lost() {
                            this.gpu_pdf.reset_device();
                        } else {
                            this.gpu_pdf.surface_zoom_live_failures =
                                this.gpu_pdf.surface_zoom_live_failures.saturating_add(1);
                            eprintln!(
                                "failed to redraw native PDF page {} sharply during zoom: {error}",
                                page + 1
                            );
                        }
                    }
                }
                if this.gpu_pdf.zoom_motion_active {
                    this.gpu_pdf.motion_deadline =
                        Some(Instant::now() + GPU_MOTION_SETTLE_DELAY);
                }
                if this.gpu_pdf.renderer.is_some() && this.gpu_pdf.rendered.contains_key(&page) {
                    let queued = this
                        .gpu_pdf
                        .motion_render_queued
                        .get_mut(&page)
                        .and_then(VecDeque::pop_front);
                    if this
                        .gpu_pdf
                        .motion_render_queued
                        .get(&page)
                        .is_some_and(VecDeque::is_empty)
                    {
                        this.gpu_pdf.motion_render_queued.remove(&page);
                    }
                    if let Some(queued) = queued
                        && let Err(error) = this.launch_native_gpu_zoom_frame(queued, cx)
                    {
                        let released = this
                            .gpu_pdf
                            .motion_render_queued
                            .remove(&page)
                            .map_or(0, |queued| queued.len() as u64);
                        this.gpu_pdf.motion_reservations_released = this
                            .gpu_pdf
                            .motion_reservations_released
                            .saturating_add(released);
                        this.gpu_pdf.surface_zoom_live_failures =
                            this.gpu_pdf.surface_zoom_live_failures.saturating_add(1);
                        eprintln!(
                            "failed to launch queued native PDF zoom redraw on page {}: {error}",
                            page + 1
                        );
                    }
                }
                cx.notify();
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to install detached native PDF zoom redraw: {error:#}");
            }
        });
        self.gpu_pdf.motion_render_tasks.insert(
            page,
            MotionRenderTask {
                page,
                generation,
                prepared,
                renderer,
                _task: task,
            },
        );
        self.gpu_pdf.motion_render_peak = self.gpu_pdf.motion_render_peak.max(1);
        self.gpu_pdf.motion_render_started = self.gpu_pdf.motion_render_started.saturating_add(1);
        Ok(())
    }

    pub(super) fn native_gpu_texture(
        &mut self,
        key: RenderKey,
        image_bounds: Bounds<Pixels>,
        viewport_bounds: Bounds<Pixels>,
        intent: GpuSurfaceIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<GpuPaintTexture> {
        let engine = cx
            .global::<SettingsStore>()
            .settings
            .document_view
            .pdf_render_engine;
        if engine == PdfRenderEngine::MuPdf {
            if self.gpu_pdf.is_active() {
                self.restore_all_native_reader_links();
                self.gpu_pdf.reset();
            }
            return None;
        }

        let page = u32::try_from(key.page_idx).ok()?;
        // The current native slice is fixed-layout PDF only. Strict mode
        // surfaces these separate writer/color-transform gaps instead of
        // silently rendering the revised page through MuPDF.
        if !self.native_gpu_key_enabled(key, engine) {
            if intent == GpuSurfaceIntent::Visible
                && engine.uses_native_reader()
                && self.format == DocumentFormat::Pdf
            {
                let message = if key.source_revision != 0 {
                    "native Glyphst rendering cannot display translated or rewritten PDF page revisions yet; the writer pipeline remains separate"
                } else if key.color_transform.is_some() {
                    "native Glyphst rendering cannot apply the reader color-transform pipeline yet"
                } else {
                    "native Glyphst rendering is unavailable for this page request"
                };
                self.gpu_pdf.surface_failures.insert(page, message.into());
            }
            return None;
        }
        let exact_visible_region = visible_gpu_region(key, image_bounds, viewport_bounds)?;
        if should_start_gpu_fallback_grace(engine, intent) {
            self.start_native_gpu_fallback_grace(page, cx);
        }

        if self.gpu_pdf.document.is_none() {
            self.start_native_gpu_document_open(cx);
            return None;
        }

        let external_context = match window.external_gpu_context() {
            Some(context) => context,
            None => {
                if self.gpu_pdf.renderer_error.is_none() {
                    let error = "the active window does not expose a same-device WGPU context";
                    eprintln!("native PDF GPU route unavailable: {error}");
                    self.gpu_pdf.renderer_error = Some(error.to_string());
                }
                return None;
            }
        };
        let token = external_context.token();
        if self.gpu_pdf.device_token.is_none() {
            self.gpu_pdf.renderer_error = None;
        }
        if self
            .gpu_pdf
            .device_token
            .is_some_and(|previous| previous != token)
        {
            self.gpu_pdf.reset_device();
        }
        self.gpu_pdf.device_token = Some(token);
        if self
            .gpu_pdf
            .renderer
            .as_ref()
            .is_some_and(|renderer| renderer.token() != token || renderer.is_device_lost())
        {
            self.gpu_pdf.reset_device();
            self.gpu_pdf.device_token = Some(token);
        }
        if self.gpu_pdf.renderer.is_none() && self.gpu_pdf.renderer_error.is_none() {
            match GpuPdfRenderer::from_gpui(&external_context) {
                Ok(renderer) => self.gpu_pdf.renderer = Some(Arc::new(renderer)),
                Err(error) => {
                    let error = format!("{error:#}");
                    eprintln!("native PDF GPU route unavailable: {error}");
                    self.gpu_pdf.renderer_error = Some(error);
                }
            }
        }
        let renderer = self.gpu_pdf.renderer.clone()?;
        let computed_target_capacity = stable_gpu_target_capacity(
            key,
            image_bounds,
            viewport_bounds,
            renderer.max_texture_dimension_2d(),
        );
        let computed_motion_target_capacity = stable_gpu_motion_target_capacity(
            key,
            image_bounds,
            viewport_bounds,
            renderer.max_texture_dimension_2d(),
        );
        let motion_target_capacity = self
            .gpu_pdf
            .zoom_motion_active
            .then(|| {
                self.gpu_pdf
                    .rendered
                    .get(&page)
                    .and_then(|rendered| rendered.presentation.motion_target_size())
            })
            .flatten()
            .or(computed_motion_target_capacity);
        let target_capacity = computed_target_capacity;

        let speculative = intent.is_speculative();
        self.gpu_pdf.mark_page_used(page);
        let prepared = match self.gpu_pdf.pages.get(&page) {
            Some(GpuPageState::Ready(page)) => Some(Arc::clone(page)),
            Some(GpuPageState::Preparing { .. })
            | Some(GpuPageState::Fallback(_))
            | Some(GpuPageState::Failed(_)) => None,
            None => {
                if self.gpu_pdf.terminal_pages.contains(&page)
                    || !should_demand_missing_gpu_page(intent)
                {
                    return None;
                }
                self.gpu_pdf.demand_page(page, false, 0);
                self.dispatch_native_gpu_page_preparations(cx);
                None
            }
        }?;

        if engine.is_strict_native() && !self.gpu_pdf.reader_link_replacements.contains_key(&page) {
            let Some(mapping) =
                self.install_native_reader_links(key.page_idx, prepared.link_interactions())
            else {
                self.gpu_pdf.application_fallbacks.insert(
                    page,
                    Arc::from([GpuFallbackDiagnostic::application_link_geometry_mismatch()]),
                );
                return None;
            };
            self.gpu_pdf
                .link_mappings
                .insert(page, CachedLinkMapping::Valid(Arc::from(mapping)));
        }
        let cached_link_mapping = self.gpu_pdf.link_mappings.get(&page);
        let link_geometry_valid = match cached_link_mapping {
            Some(CachedLinkMapping::Valid(_)) => true,
            Some(CachedLinkMapping::Invalid) => false,
            None => {
                let mapping = Self::native_link_mapping(
                    key.page_idx,
                    &self.links,
                    prepared.link_interactions(),
                );
                if !self.text_index_state.is_pending_or_indexing() {
                    self.gpu_pdf.link_mappings.insert(
                        page,
                        mapping
                            .as_ref()
                            .map_or(CachedLinkMapping::Invalid, |mapping| {
                                CachedLinkMapping::Valid(Arc::from(mapping.as_slice()))
                            }),
                    );
                }
                mapping.is_some()
            }
        };
        if !link_geometry_valid {
            if !self.text_index_state.is_pending_or_indexing() {
                self.gpu_pdf.application_fallbacks.insert(
                    page,
                    Arc::from([GpuFallbackDiagnostic::application_link_geometry_mismatch()]),
                );
            } else {
                self.gpu_pdf.application_fallbacks.remove(&page);
            }
            return None;
        }
        self.gpu_pdf.application_fallbacks.remove(&page);

        if speculative {
            self.enqueue_native_gpu_continuity_surface(
                page,
                key,
                Arc::clone(&prepared),
                Arc::clone(&renderer),
                true,
                cx,
            );
            return None;
        }

        let settled = !self.dock_snapshot.layout_motion_active
            && self.panning.is_none()
            && !self.gpu_pdf.motion_active
            && (self.scroll_x_doc - self.target_scroll_x_doc).abs() <= 0.01
            && (self.scroll_y_doc - self.target_scroll_y_doc).abs() <= 0.01;
        // Compare the live layout with the retained page in the retained
        // page's own physical coordinate space. Scale-changing motion takes
        // the direct current-camera path below instead of sampling these
        // completed pixels at a different density.
        let current_surface = self.gpu_pdf.rendered.get(&page).and_then(|rendered| {
            if !same_gpu_page_source(rendered.key, key)
                || rendered.presentation.token() != Some(renderer.token())
            {
                return None;
            }
            let visible_region = visible_gpu_region(rendered.key, image_bounds, viewport_bounds)?;
            let current_request = gpu_paint_request(
                rendered.key,
                visible_region,
                settled,
                target_capacity,
                motion_target_capacity,
            );
            let target_region = rendered.presentation.surface_region(current_request).ok();
            Some(CurrentGpuSurface {
                key: rendered.key,
                request: rendered.request,
                refresh_region: rendered.refresh_region,
                visible_region,
                target_region,
                texture: rendered.presentation.current_texture()?,
                refinement_pending: rendered.presentation.refinement_pending(),
                prewarmed: rendered.prewarmed,
            })
        });
        let current_covers = current_surface.as_ref().is_some_and(|current| {
            gpu_region_contains(current.texture.region(), current.visible_region)
        });
        let current_quality = current_surface
            .as_ref()
            .is_some_and(|current| motion_surface_density_sufficient(current.key, key));
        let current_dimensions_exact = current_surface
            .as_ref()
            .is_some_and(|current| same_gpu_page_dimensions(current.key, key));
        if current_surface.is_some() && !self.gpu_pdf.zoom_motion_active {
            self.enqueue_native_gpu_continuity_surface(
                page,
                key,
                Arc::clone(&prepared),
                Arc::clone(&renderer),
                false,
                cx,
            );
        }
        let (selected_surface, continuity_covers) = self.selected_gpu_surface(
            page,
            key,
            image_bounds,
            viewport_bounds,
            renderer.token(),
            current_surface.as_ref(),
        );

        if should_present_continuity_motion(
            settled,
            current_dimensions_exact,
            self.gpu_pdf.zoom_motion_active,
            selected_surface.as_ref().map(|selected| selected.kind),
        ) {
            // A prewarmed full-page continuity surface remains the fixed-scale
            // scroll frame. A scale-changing gesture must proceed to the
            // current-camera retained redraw below instead of sampling this
            // texture at a different density.
            return selected_surface
                .map(|selected| self.present_gpu_surface(page, image_bounds, selected));
        }

        let dimension_motion = !settled && !current_dimensions_exact;
        if dimension_motion && self.gpu_pdf.zoom_motion_active {
            let request = gpu_paint_request(
                key,
                exact_visible_region,
                false,
                target_capacity,
                motion_target_capacity,
            );
            match self.start_native_gpu_zoom_frame(
                page,
                key,
                Arc::clone(&prepared),
                Arc::clone(&renderer),
                request,
                cx,
            ) {
                Ok(_) => return self.hold_last_sharp_gpu_texture(page),
                Err(error) => {
                    self.gpu_pdf.surface_zoom_live_failures =
                        self.gpu_pdf.surface_zoom_live_failures.saturating_add(1);
                    if renderer.is_device_lost() {
                        self.gpu_pdf.reset_device();
                        return None;
                    }
                    eprintln!(
                        "failed to redraw native PDF page {} sharply during zoom: {error}",
                        page + 1
                    );
                    return self.hold_last_sharp_gpu_texture(page);
                }
            }
        }

        if intent == GpuSurfaceIntent::Visible && current_covers {
            self.gpu_pdf.covered_pages.insert(page);
            if let Some(current) = current_surface.as_ref()
                && prewarmed_surface_covers(
                    current.prewarmed,
                    current.key,
                    current.texture.region(),
                    key,
                    current.visible_region,
                    true,
                )
            {
                self.gpu_pdf.surface_warm_hits = self.gpu_pdf.surface_warm_hits.saturating_add(1);
                if let Some(rendered) = self.gpu_pdf.rendered.get_mut(&page) {
                    rendered.prewarmed = false;
                }
            }
        }

        if should_defer_fixed_scale_scroll_refresh(
            settled,
            current_dimensions_exact,
            current_covers,
            continuity_covers,
        ) {
            if current_surface
                .as_ref()
                .is_some_and(|current| current.request.visible_region != current.visible_region)
            {
                self.gpu_pdf.surface_scroll_reuses =
                    self.gpu_pdf.surface_scroll_reuses.saturating_add(1);
            }
            if intent == GpuSurfaceIntent::Visible {
                let selected = selected_surface.expect("covered primary surface was selected");
                return Some(self.present_gpu_surface(page, image_bounds, selected));
            }
            return None;
        }

        // Fixed-scale scrolling stops here while its overscan remains safe.
        // Interactive scale motion already returned through the direct
        // current-camera path above; this density check is only for other
        // unsettled layout changes such as dock resizing.
        let current_directly_reusable = current_surface.as_ref().is_some_and(|current| {
            current_covers
                && (!settled || (current.request.settled && !current.refinement_pending))
                && (if settled {
                    current_dimensions_exact
                } else {
                    current_quality
                })
                && (gpu_region_contains(current.refresh_region, current.visible_region)
                    || current
                        .target_region
                        .is_some_and(|target| current.texture.region() == target))
        });
        if current_directly_reusable {
            let current = current_surface.as_ref().expect("checked retained surface");
            if current.request.visible_region != current.visible_region {
                if current_dimensions_exact {
                    self.gpu_pdf.surface_scroll_reuses =
                        self.gpu_pdf.surface_scroll_reuses.saturating_add(1);
                } else {
                    self.gpu_pdf.surface_zoom_reuses =
                        self.gpu_pdf.surface_zoom_reuses.saturating_add(1);
                }
            } else if !current_dimensions_exact {
                self.gpu_pdf.surface_zoom_reuses =
                    self.gpu_pdf.surface_zoom_reuses.saturating_add(1);
            }
            if intent == GpuSurfaceIntent::Visible {
                let selected = selected_surface.expect("covered primary surface was selected");
                return Some(self.present_gpu_surface(page, image_bounds, selected));
            }
            return None;
        }

        let render_key = key;
        let refresh_kind = match current_surface.as_ref() {
            Some(_) if settled && !current_dimensions_exact => SurfaceRefreshKind::SettledExact,
            Some(_) if settled && current_covers => SurfaceRefreshKind::Prefetch,
            Some(_) if settled => SurfaceRefreshKind::Urgent,
            Some(_) if dimension_motion && self.gpu_pdf.zoom_motion_active => {
                SurfaceRefreshKind::ZoomEmergency
            }
            Some(_) if dimension_motion => SurfaceRefreshKind::Urgent,
            Some(_) if current_covers => SurfaceRefreshKind::Prefetch,
            Some(_) => SurfaceRefreshKind::Urgent,
            None => SurfaceRefreshKind::Initial,
        };
        let visible_region = exact_visible_region;
        let request = gpu_paint_request(
            render_key,
            visible_region,
            settled,
            target_capacity,
            motion_target_capacity,
        );
        let surface_region = match self.gpu_pdf.rendered.get(&page) {
            Some(rendered) => rendered.presentation.surface_region(request),
            None => request.surface_region(),
        };
        let surface_region = match surface_region {
            Ok(region) => region,
            Err(error) => {
                if intent == GpuSurfaceIntent::Visible {
                    self.gpu_pdf
                        .surface_failures
                        .insert(page, format!("{error:#}"));
                }
                return self.present_selected_or_record_gap(page, image_bounds, selected_surface);
            }
        };

        let vector_lod_status = match renderer.vector_lod_status(&prepared, request) {
            Ok(status) => status,
            Err(error) => {
                if intent == GpuSurfaceIntent::Visible {
                    self.gpu_pdf
                        .surface_failures
                        .insert(page, format!("{error:#}"));
                }
                return self.present_selected_or_record_gap(page, image_bounds, selected_surface);
            }
        };
        if vector_lod_status.ready {
            if let Some(task) = self.gpu_pdf.vector_lod_tasks.remove(&page) {
                task.cancellation.cancel();
            }
            self.gpu_pdf.vector_lod_failures.remove(&page);
        } else {
            if speculative {
                // Speculative work consumes only existing retained resources.
                return None;
            }
            if settled {
                if let Some(required_scale) = next_vector_lod_scale(
                    vector_lod_status.available_scale,
                    vector_lod_status.requested_scale,
                ) {
                    self.start_native_gpu_vector_lod_prepare(
                        page,
                        Arc::clone(&prepared),
                        Arc::clone(&renderer),
                        required_scale,
                        cx,
                    );
                }
                return self.present_selected_or_record_gap(page, image_bounds, selected_surface);
            }
        }

        if vector_lod_status.ready {
            let patch_lod_status = match renderer.patch_lod_status(&prepared, request) {
                Ok(status) => status,
                Err(error) => {
                    if intent == GpuSurfaceIntent::Visible {
                        self.gpu_pdf
                            .surface_failures
                            .insert(page, format!("{error:#}"));
                    }
                    return self.present_selected_or_record_gap(
                        page,
                        image_bounds,
                        selected_surface,
                    );
                }
            };
            if patch_lod_status.ready {
                if let Some(task) = self.gpu_pdf.patch_lod_tasks.remove(&page) {
                    task.cancellation.cancel();
                }
            } else if speculative {
                // Surface warming consumes only already-prepared GPU resources.
                // It must not create patch-LOD work that can contend with visible
                // pages or prolong a speculative page's lifetime.
                return None;
            } else {
                self.start_native_gpu_patch_lod_prepare(
                    page,
                    Arc::clone(&prepared),
                    Arc::clone(&renderer),
                    request,
                    patch_lod_status.required_scale,
                    cx,
                );
                if settled {
                    return self.present_selected_or_record_gap(
                        page,
                        image_bounds,
                        selected_surface,
                    );
                }
            }
        }

        // GPU command recording and submission never run inside GPUI's paint
        // traversal. Keep presenting the prior same-device texture while the
        // single latest-request worker renders this update.
        self.enqueue_native_gpu_surface_render(
            page,
            render_key,
            Arc::clone(&prepared),
            Arc::clone(&renderer),
            request,
            surface_region,
            SurfaceRenderRole::Primary,
            speculative,
            refresh_kind,
            None,
            cx,
        );
        if let Some(selected) = selected_surface {
            if matches!(
                selected.kind,
                PresentedSurfaceKind::Primary | PresentedSurfaceKind::PartialPrimary
            ) && !same_gpu_page_dimensions(selected.key, key)
            {
                self.gpu_pdf.surface_zoom_reuses =
                    self.gpu_pdf.surface_zoom_reuses.saturating_add(1);
            }
            return Some(self.present_gpu_surface(page, image_bounds, selected));
        }
        self.present_selected_or_record_gap(page, image_bounds, None)
    }

    fn start_native_gpu_document_open(&mut self, cx: &mut Context<Self>) {
        if self.gpu_pdf.document_task.is_some() || self.gpu_pdf.document_error.is_some() {
            return;
        }
        let snapshot = Arc::clone(&self.document_snapshot);
        let expected_page_count = self.page_count;
        let generation = self.gpu_pdf.generation;
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    GpuPdfDocument::open(&snapshot)
                        .map(Arc::new)
                        .map_err(|error| format!("{error:#}"))
                })
                .await;
            let update_result = this.update(cx, |this, cx| {
                if this.gpu_pdf.generation != generation {
                    return;
                }
                this.gpu_pdf.document_task = None;
                match result {
                    Ok(document) if document.page_count() as usize == expected_page_count => {
                        match document.navigation_route() {
                            GpuNavigationRoute::Ready(outline) => {
                                this.gpu_pdf.navigation_fallback = None;
                                this.install_native_gpu_outline(&outline, cx);
                            }
                            GpuNavigationRoute::Fallback(diagnostics) => {
                                for diagnostic in diagnostics.iter() {
                                    eprintln!(
                                        "native PDF navigation fallback [{}:{}]: {}",
                                        diagnostic.feature, diagnostic.code, diagnostic.message
                                    );
                                }
                                this.gpu_pdf.navigation_fallback = Some(diagnostics);
                            }
                        }
                        this.gpu_pdf.document = Some(document);
                    }
                    Ok(document) => {
                        let error = format!(
                            "page count differs between MuPDF ({expected_page_count}) and the native engine ({})",
                            document.page_count()
                        );
                        eprintln!("native PDF GPU route unavailable: {error}");
                        this.gpu_pdf.document_error = Some(error);
                    }
                    Err(error) => {
                        eprintln!("native PDF GPU route unavailable: {error}");
                        this.gpu_pdf.document_error = Some(error);
                    }
                }
                cx.notify();
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to install native PDF document: {error:#}");
            }
        });
        self.gpu_pdf.document_task = Some(task);
        cx.notify();
    }

    fn start_native_gpu_page_prepare(
        &mut self,
        page: u32,
        demand: GpuPreparationDemand,
        document: Arc<GpuPdfDocument>,
        renderer: Arc<GpuPdfRenderer>,
        cx: &mut Context<Self>,
    ) {
        if self.gpu_pdf.pages.contains_key(&page) {
            return;
        }
        let cancellation = GpuCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let generation = self.gpu_pdf.generation;
        self.gpu_pdf.preparation_generation =
            self.gpu_pdf.preparation_generation.wrapping_add(1).max(1);
        let preparation_generation = self.gpu_pdf.preparation_generation;
        let token = renderer.token();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    renderer
                        .prepare_page(&document, page, &worker_cancellation)
                        .map_err(|error| format!("{error:#}"))
                })
                .await;
            let update_result = this.update(cx, |this, cx| {
                if this.gpu_pdf.generation != generation
                    || !matches!(
                        this.gpu_pdf.pages.get(&page),
                        Some(GpuPageState::Preparing { generation, .. })
                            if *generation == preparation_generation
                    )
                    || this
                        .gpu_pdf
                        .renderer
                        .as_ref()
                        .is_none_or(|renderer| renderer.token() != token)
                {
                    return;
                }
                this.gpu_pdf.preparation_demands.remove(&page);
                this.gpu_pdf.preparation_completed =
                    this.gpu_pdf.preparation_completed.saturating_add(1);
                match result {
                    Ok(GpuPageRoute::Ready(prepared)) => {
                        this.gpu_pdf.fallback_grace_tasks.remove(&page);
                        this.gpu_pdf.fallback_grace_elapsed.remove(&page);
                        this.gpu_pdf
                            .pages
                            .insert(page, GpuPageState::Ready(prepared));
                    }
                    Ok(GpuPageRoute::Fallback(diagnostics)) => {
                        this.gpu_pdf.fallback_grace_tasks.remove(&page);
                        this.gpu_pdf.fallback_grace_elapsed.remove(&page);
                        for diagnostic in diagnostics.iter() {
                            eprintln!(
                                "native PDF page {} fallback [{}:{}]: {}",
                                page + 1,
                                diagnostic.feature,
                                diagnostic.code,
                                diagnostic.message
                            );
                        }
                        this.gpu_pdf
                            .pages
                            .insert(page, GpuPageState::Fallback(diagnostics));
                        this.gpu_pdf.terminal_pages.insert(page);
                    }
                    Err(error) => {
                        this.gpu_pdf.fallback_grace_tasks.remove(&page);
                        this.gpu_pdf.fallback_grace_elapsed.remove(&page);
                        eprintln!(
                            "failed to prepare native PDF page {} for the GPU: {error}",
                            page + 1
                        );
                        this.gpu_pdf.pages.insert(page, GpuPageState::Failed(error));
                        this.gpu_pdf.terminal_pages.insert(page);
                    }
                }
                this.dispatch_native_gpu_page_preparations(cx);
                cx.notify();
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to install native PDF GPU page: {error:#}");
            }
        });
        self.gpu_pdf.pages.insert(
            page,
            GpuPageState::Preparing {
                generation: preparation_generation,
                cancellation,
                demand,
                _task: task,
            },
        );
        cx.notify();
    }

    fn native_gpu_preparation_motion_active(&self) -> bool {
        self.dock_snapshot.layout_motion_active
            || self.panning.is_some()
            || self.gpu_pdf.motion_active
            || (self.scroll_x_doc - self.target_scroll_x_doc).abs() > 0.01
            || (self.scroll_y_doc - self.target_scroll_y_doc).abs() > 0.01
    }

    fn dispatch_native_gpu_page_preparations(&mut self, cx: &mut Context<Self>) {
        let (Some(document), Some(renderer)) =
            (self.gpu_pdf.document.clone(), self.gpu_pdf.renderer.clone())
        else {
            return;
        };
        let motion_active = self.native_gpu_preparation_motion_active();
        let speculative_background_paused =
            motion_active || self.gpu_pdf.speculative_resume_task.is_some();
        while speculative_background_paused && self.gpu_pdf.active_speculative_preparations() > 0 {
            let Some(page) = self.gpu_pdf.worst_speculative_preparation() else {
                break;
            };
            self.gpu_pdf.cancel_active_preparation(page, true);
        }

        loop {
            let active = self.gpu_pdf.active_preparations();
            let active_speculative = self.gpu_pdf.active_speculative_preparations();
            let candidate = self
                .gpu_pdf
                .preparation_demands
                .iter()
                .filter(|(page, _)| {
                    !self.gpu_pdf.pages.contains_key(page)
                        && !self.gpu_pdf.terminal_pages.contains(page)
                })
                .filter(|(_, demand)| {
                    !demand.speculative
                        || speculative_preparation_allowed(
                            motion_active,
                            speculative_background_paused,
                            active_speculative,
                        )
                })
                .min_by_key(|(page, demand)| (demand.priority, demand.sequence, **page))
                .map(|(page, demand)| (*page, *demand));
            let Some((page, demand)) = candidate else {
                break;
            };

            if active >= MAX_CONCURRENT_GPU_PAGE_PREPARATIONS {
                if !demand.speculative
                    && let Some(preempted) = self.gpu_pdf.worst_speculative_preparation()
                {
                    self.gpu_pdf.cancel_active_preparation(preempted, true);
                    continue;
                }
                break;
            }
            let retained_pages = self
                .gpu_pdf
                .pages
                .values()
                .filter(|state| {
                    matches!(
                        state,
                        GpuPageState::Preparing { .. } | GpuPageState::Ready(_)
                    )
                })
                .count();
            if demand.speculative && retained_pages >= MAX_RETAINED_GPU_PAGES {
                break;
            }

            self.gpu_pdf.mark_page_used(page);
            self.start_native_gpu_page_prepare(
                page,
                demand,
                Arc::clone(&document),
                Arc::clone(&renderer),
                cx,
            );
        }
    }

    fn enqueue_native_gpu_continuity_surface(
        &mut self,
        page: u32,
        requested_key: RenderKey,
        prepared: Arc<PreparedGpuPage>,
        renderer: Arc<GpuPdfRenderer>,
        speculative: bool,
        cx: &mut Context<Self>,
    ) {
        if self
            .gpu_pdf
            .continuity_surfaces
            .get(&page)
            .is_some_and(|continuity| {
                same_gpu_page_source(continuity.key, requested_key)
                    && continuity.texture.token() == renderer.token()
            })
        {
            return;
        }
        if self
            .gpu_pdf
            .surface_seed_tasks
            .get(&page)
            .is_some_and(|task| task.role == SurfaceRenderRole::Primary)
            || self
                .gpu_pdf
                .surface_render_pending
                .get(&page)
                .is_some_and(|pending| pending.role == SurfaceRenderRole::Primary)
        {
            return;
        }

        let key = continuity_render_key(requested_key);
        let visible_region = full_page_gpu_region(key);
        let request = gpu_paint_request(key, visible_region, false, None, None);
        self.enqueue_native_gpu_surface_render(
            page,
            key,
            prepared,
            renderer,
            request,
            visible_region,
            SurfaceRenderRole::Continuity,
            speculative,
            SurfaceRefreshKind::Initial,
            None,
            cx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_native_gpu_surface_render(
        &mut self,
        page: u32,
        key: RenderKey,
        prepared: Arc<PreparedGpuPage>,
        renderer: Arc<GpuPdfRenderer>,
        mut request: GpuPaintRequest,
        region: GpuPageRegion,
        role: SurfaceRenderRole,
        speculative: bool,
        refresh_kind: SurfaceRefreshKind,
        chain_started_at: Option<Instant>,
        cx: &mut Context<Self>,
    ) {
        if self
            .gpu_pdf
            .surface_render_pending
            .get(&page)
            .is_some_and(|pending| {
                pending.role == role
                    && can_coalesce_pending_surface_request(
                        SurfaceRequestIdentity::new(pending.key, pending.request, pending.region),
                        pending.speculative,
                        SurfaceRequestIdentity::new(key, request, region),
                        speculative,
                    )
            })
        {
            return;
        }

        let active_for_page = self.gpu_pdf.surface_seed_tasks.get(&page).map(|active| {
            (
                surface_request_covers(
                    SurfaceRequestIdentity::new(active.key, active.request, active.region),
                    SurfaceRequestIdentity::new(key, request, region),
                ),
                Arc::clone(&active.prepared),
                Arc::clone(&active.renderer),
                active.role,
                active.speculative,
            )
        });
        if matches!(
            active_for_page,
            Some((same_request, _, _, active_role, active_speculative))
                if can_reuse_active_surface_request(
                    same_request && active_role == role,
                    active_speculative,
                    speculative,
                )
        ) && !self.gpu_pdf.surface_render_pending.contains_key(&page)
        {
            return;
        }

        assign_surface_render_generation(&mut self.gpu_pdf.render_generation, &mut request);
        if let Some((_, active_page, active_renderer, _, _)) = active_for_page {
            self.gpu_pdf.surface_render_coalesced =
                self.gpu_pdf.surface_render_coalesced.saturating_add(1);
            if let Err(error) =
                active_renderer.supersede_page_render(&active_page, request.generation)
            {
                self.gpu_pdf
                    .surface_failures
                    .insert(page, format!("{error:#}"));
                if active_renderer.is_device_lost() {
                    self.gpu_pdf.reset_device();
                }
                return;
            }
            if let Some(active) = self.gpu_pdf.surface_seed_tasks.get_mut(&page) {
                active.superseded = true;
            }
        }

        if self.gpu_pdf.surface_render_pending.contains_key(&page) {
            self.gpu_pdf.surface_render_coalesced =
                self.gpu_pdf.surface_render_coalesced.saturating_add(1);
        }
        self.gpu_pdf.surface_render_sequence =
            self.gpu_pdf.surface_render_sequence.wrapping_add(1).max(1);
        let sequence = self.gpu_pdf.surface_render_sequence;
        match refresh_kind {
            SurfaceRefreshKind::Initial => {}
            SurfaceRefreshKind::Prefetch => {
                self.gpu_pdf.surface_scroll_prefetches =
                    self.gpu_pdf.surface_scroll_prefetches.saturating_add(1);
            }
            SurfaceRefreshKind::Urgent => {
                self.gpu_pdf.surface_scroll_misses =
                    self.gpu_pdf.surface_scroll_misses.saturating_add(1);
            }
            SurfaceRefreshKind::ZoomEmergency => {
                self.gpu_pdf.surface_zoom_emergency_renders = self
                    .gpu_pdf
                    .surface_zoom_emergency_renders
                    .saturating_add(1);
            }
            SurfaceRefreshKind::SettledExact => {
                self.gpu_pdf.surface_zoom_settled_upgrades =
                    self.gpu_pdf.surface_zoom_settled_upgrades.saturating_add(1);
            }
            SurfaceRefreshKind::RefinementContinuation => {}
        }
        if speculative {
            self.gpu_pdf.surface_warm_requests =
                self.gpu_pdf.surface_warm_requests.saturating_add(1);
        }
        if role == SurfaceRenderRole::Continuity {
            self.gpu_pdf.surface_continuity_requests =
                self.gpu_pdf.surface_continuity_requests.saturating_add(1);
        }
        let enqueued_at = Instant::now();
        self.gpu_pdf.surface_render_pending.insert(
            page,
            PendingSurfaceRender {
                sequence,
                key,
                region,
                prepared,
                renderer,
                request,
                role,
                speculative,
                enqueued_at,
                chain_started_at: chain_started_at.unwrap_or(enqueued_at),
            },
        );
        self.gpu_pdf.surface_pending_peak = self
            .gpu_pdf
            .surface_pending_peak
            .max(self.gpu_pdf.surface_render_pending.len());

        // A visible primary surface outranks continuity and speculative
        // adjacent-page work. The engine observes this generation atomically
        // and abandons the old command stream at its next safe checkpoint.
        if role == SurfaceRenderRole::Primary && !speculative {
            let preemptible_active = self
                .gpu_pdf
                .surface_seed_tasks
                .iter()
                .find(|(active_page, active)| {
                    **active_page != page
                        && (active.speculative || active.role == SurfaceRenderRole::Continuity)
                        && !active.superseded
                })
                .map(|(active_page, active)| {
                    (
                        *active_page,
                        Arc::clone(&active.prepared),
                        Arc::clone(&active.renderer),
                    )
                });
            if let Some((active_page, active_prepared, active_renderer)) = preemptible_active {
                match active_renderer.supersede_page_render(&active_prepared, request.generation) {
                    Ok(()) => {
                        if let Some(active) = self.gpu_pdf.surface_seed_tasks.get_mut(&active_page)
                        {
                            active.superseded = true;
                        }
                    }
                    Err(error) => {
                        self.gpu_pdf
                            .surface_failures
                            .insert(active_page, format!("{error:#}"));
                        if active_renderer.is_device_lost() {
                            self.gpu_pdf.reset_device();
                            return;
                        }
                    }
                }
            }
        }

        self.dispatch_native_gpu_surface_render(cx);
    }

    fn dispatch_native_gpu_surface_render(&mut self, cx: &mut Context<Self>) {
        if self.gpu_pdf.zoom_motion_active
            || !self.gpu_pdf.motion_render_tasks.is_empty()
            || !self.gpu_pdf.motion_render_queued.is_empty()
            || !self.gpu_pdf.surface_seed_tasks.is_empty()
            || !self.gpu_pdf.vector_lod_tasks.is_empty()
        {
            return;
        }

        loop {
            let Some(page) = self
                .gpu_pdf
                .surface_render_pending
                .iter()
                .filter(|(_, pending)| {
                    !self.native_gpu_preparation_motion_active()
                        || (pending.role == SurfaceRenderRole::Primary && !pending.speculative)
                })
                .min_by_key(|(page, pending)| {
                    pending_surface_priority(
                        **page,
                        pending.role,
                        pending.speculative,
                        pending.sequence,
                    )
                })
                .map(|(page, _)| *page)
            else {
                return;
            };
            let Some(pending) = self.gpu_pdf.surface_render_pending.remove(&page) else {
                continue;
            };
            if self
                .gpu_pdf
                .renderer
                .as_ref()
                .is_none_or(|renderer| renderer.token() != pending.renderer.token())
                || !matches!(
                    self.gpu_pdf.pages.get(&page),
                    Some(GpuPageState::Ready(current))
                        if Arc::ptr_eq(current, &pending.prepared)
                )
            {
                self.gpu_pdf.surface_render_coalesced =
                    self.gpu_pdf.surface_render_coalesced.saturating_add(1);
                continue;
            }
            let presentation = if pending.role == SurfaceRenderRole::Primary {
                self.gpu_pdf
                    .rendered
                    .get(&page)
                    .map(|rendered| rendered.presentation.clone())
                    .unwrap_or_default()
            } else {
                GpuPagePresentation::default()
            };
            let region = pending.region;
            let already_rendered = match pending.role {
                SurfaceRenderRole::Primary => {
                    self.gpu_pdf.rendered.get(&page).is_some_and(|rendered| {
                        rendered
                            .presentation
                            .current_texture()
                            .is_some_and(|texture| {
                                surface_request_covers(
                                    SurfaceRequestIdentity::new(
                                        rendered.key,
                                        rendered.request,
                                        texture.region(),
                                    ),
                                    SurfaceRequestIdentity::new(
                                        pending.key,
                                        pending.request,
                                        region,
                                    ),
                                )
                            })
                            && rendered.presentation.token() == Some(pending.renderer.token())
                            && (!pending.request.settled
                                || !rendered.presentation.refinement_pending())
                    })
                }
                SurfaceRenderRole::Continuity => self
                    .gpu_pdf
                    .continuity_surfaces
                    .get(&page)
                    .is_some_and(|continuity| {
                        continuity.key == pending.key
                            && continuity.texture.token() == pending.renderer.token()
                            && continuity.texture.region() == region
                    }),
            };
            if already_rendered {
                self.gpu_pdf.surface_failures.remove(&page);
                continue;
            }
            let has_previous = presentation.has_working_texture();
            let can_defer_full_render = pending.role == SurfaceRenderRole::Primary
                && self.gpu_pdf.rendered.get(&page).is_some_and(|rendered| {
                    rendered.key == pending.key
                        && same_gpu_surface_content(rendered.request, pending.request)
                        && rendered.presentation.token() == Some(pending.renderer.token())
                        && rendered
                            .presentation
                            .current_texture()
                            .is_some_and(|texture| {
                                gpu_region_contains(
                                    texture.region(),
                                    pending.request.visible_region,
                                )
                            })
                });
            let options = GpuPageRenderOptions {
                defer_full_render: should_defer_full_surface_render(
                    pending.request.settled,
                    pending.speculative,
                    has_previous,
                ) && can_defer_full_render,
            };

            self.start_native_gpu_surface_seed(page, pending, region, options, presentation, cx);
            return;
        }
    }

    fn start_native_gpu_surface_seed(
        &mut self,
        page: u32,
        pending: PendingSurfaceRender,
        region: GpuPageRegion,
        options: GpuPageRenderOptions,
        mut presentation: GpuPagePresentation,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(self.gpu_pdf.surface_seed_tasks.is_empty());
        let document_generation = self.gpu_pdf.generation;
        self.gpu_pdf.surface_seed_generation =
            self.gpu_pdf.surface_seed_generation.wrapping_add(1).max(1);
        let generation = self.gpu_pdf.surface_seed_generation;
        let PendingSurfaceRender {
            key,
            prepared,
            renderer,
            request,
            role,
            speculative,
            enqueued_at,
            chain_started_at,
            ..
        } = pending;
        let token = renderer.token();
        let worker_page = Arc::clone(&prepared);
        let worker_renderer = Arc::clone(&renderer);
        let completion_page = Arc::clone(&prepared);
        let completion_renderer = Arc::clone(&renderer);
        let started_at = Instant::now();
        let in_flight_bytes = region.byte_len();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    worker_renderer
                        .render_page(&worker_page, request, options, &mut presentation)
                        .map(|outcome| (presentation, outcome))
                        .map_err(|error| format!("{error:#}"))
                })
                .await;
            let finished_at = Instant::now();
            let update_result = this.update(cx, |this, cx| {
                if this.gpu_pdf.generation != document_generation
                    || !matches!(
                        this.gpu_pdf.surface_seed_tasks.get(&page),
                        Some(task)
                            if task.generation == generation
                                && task.key == key
                                && task.region == region
                                && task.role == role
                    )
                {
                    return;
                }
                let Some(completed_task) = this.gpu_pdf.surface_seed_tasks.remove(&page) else {
                    return;
                };
                let completed_enqueued_at = completed_task.enqueued_at;
                let completed_chain_started_at = completed_task.chain_started_at;
                this.gpu_pdf.record_surface_latency(
                    completed_task
                        .started_at
                        .saturating_duration_since(completed_enqueued_at),
                    finished_at.saturating_duration_since(completed_enqueued_at),
                );
                this.gpu_pdf.surface_render_completed =
                    this.gpu_pdf.surface_render_completed.saturating_add(1);
                if this
                    .gpu_pdf
                    .renderer
                    .as_ref()
                    .is_none_or(|renderer| renderer.token() != token)
                    || !matches!(
                        this.gpu_pdf.pages.get(&page),
                        Some(GpuPageState::Ready(current))
                            if Arc::ptr_eq(current, &completion_page)
                    )
                {
                    this.gpu_pdf.surface_render_discarded =
                        this.gpu_pdf.surface_render_discarded.saturating_add(1);
                    this.dispatch_native_gpu_surface_render(cx);
                    return;
                }
                if completed_task.superseded {
                    if matches!(
                        &result,
                        Ok((_, GpuPageRenderOutcome::Superseded))
                    ) {
                        this.gpu_pdf.surface_render_superseded =
                            this.gpu_pdf.surface_render_superseded.saturating_add(1);
                    } else {
                        // Work that crossed its final cancellation checkpoint
                        // may still finish on the GPU. Never publish those
                        // stale pixels after a newer zoom gesture began.
                        this.gpu_pdf.surface_render_discarded =
                            this.gpu_pdf.surface_render_discarded.saturating_add(1);
                    }
                    this.dispatch_native_gpu_surface_render(cx);
                    return;
                }
                let newer_pending = this
                    .gpu_pdf
                    .surface_render_pending
                    .get(&page)
                    .is_some_and(|pending| {
                        pending.role == role
                            && !surface_request_covers(
                                SurfaceRequestIdentity::new(key, request, region),
                                SurfaceRequestIdentity::new(
                                    pending.key,
                                    pending.request,
                                    pending.region,
                                ),
                            )
                });
                let mut notify = false;
                let mut continue_refinement = false;
                match result {
                    Ok((presentation, GpuPageRenderOutcome::Rendered { presented, stats })) => {
                        if role == SurfaceRenderRole::Continuity {
                            if let Some(texture) = presented {
                                let bytes = texture.byte_len();
                                if this.gpu_pdf.reserve_continuity_surface(page, bytes) {
                                    this.gpu_pdf.continuity_surfaces.insert(
                                        page,
                                        ContinuityGpuSurface {
                                            key,
                                            texture,
                                            stats,
                                            enqueued_at: completed_chain_started_at,
                                            first_paint_recorded: false,
                                            prewarmed: speculative
                                                && !this.gpu_pdf.visible_pages.contains(&page),
                                        },
                                    );
                                    this.gpu_pdf.surface_continuity_completed = this
                                        .gpu_pdf
                                        .surface_continuity_completed
                                        .saturating_add(1);
                                    this.gpu_pdf.surface_failures.remove(&page);
                                    this.gpu_pdf.prune_surface_cache();
                                    notify = true;
                                } else {
                                    this.gpu_pdf.surface_render_cache_pressure = this
                                        .gpu_pdf
                                        .surface_render_cache_pressure
                                        .saturating_add(1);
                                    this.gpu_pdf.surface_render_discarded = this
                                        .gpu_pdf
                                        .surface_render_discarded
                                        .saturating_add(1);
                                }
                            } else {
                                this.gpu_pdf.surface_render_discarded =
                                    this.gpu_pdf.surface_render_discarded.saturating_add(1);
                            }
                        } else {
                            let was_refining = this
                                .gpu_pdf
                                .rendered
                                .get(&page)
                                .is_some_and(|rendered| {
                                    rendered.presentation.refinement_pending()
                                });
                            let has_current = this
                                .gpu_pdf
                                .rendered
                                .get(&page)
                                .is_some_and(|rendered| {
                                    rendered.presentation.current_texture().is_some()
                                })
                                || this.gpu_pdf.continuity_surfaces.contains_key(&page);
                            if newer_pending && has_current {
                                this.gpu_pdf.surface_render_discarded = this
                                    .gpu_pdf
                                    .surface_render_discarded
                                    .saturating_add(1);
                            } else {
                                let incomplete = presented.is_none()
                                    && presentation.refinement_pending();
                                let bytes = presentation.byte_len();
                                if let Err(required) = this.gpu_pdf.reserve_surface(page, bytes) {
                                    this.gpu_pdf.surface_render_cache_pressure = this
                                        .gpu_pdf
                                        .surface_render_cache_pressure
                                        .saturating_add(1);
                                    this.gpu_pdf.surface_render_discarded = this
                                        .gpu_pdf
                                        .surface_render_discarded
                                        .saturating_add(1);
                                    match surface_cache_pressure_action(speculative, has_current) {
                                        SurfaceCachePressureAction::PreserveDiagnostics => {}
                                        SurfaceCachePressureAction::ClearDiagnostics => {
                                            // Cache pressure must not route a page with a valid
                                            // retained texture back to MuPDF.
                                            this.gpu_pdf.surface_failures.remove(&page);
                                        }
                                        SurfaceCachePressureAction::RouteToFallback => {
                                            this.gpu_pdf.surface_failures.insert(
                                                page,
                                                format!(
                                                    "visible native PDF surfaces require {} bytes, exceeding the {}-byte cache limit",
                                                    required, MAX_RETAINED_GPU_SURFACE_BYTES
                                                ),
                                            );
                                            notify = true;
                                        }
                                    }
                                    // A speculative reservation failure neither creates nor
                                    // clears page-routing diagnostics.
                                } else {
                                    if speculative {
                                        let stale_prewarmed = this
                                            .gpu_pdf
                                            .rendered
                                            .iter()
                                            .filter_map(|(candidate, rendered)| {
                                                (*candidate != page
                                                    && rendered.prewarmed
                                                    && !this
                                                        .gpu_pdf
                                                        .visible_pages
                                                        .contains(candidate))
                                                .then_some(*candidate)
                                            })
                                            .collect::<Vec<_>>();
                                        for candidate in stale_prewarmed {
                                            this.gpu_pdf.rendered.remove(&candidate);
                                        }
                                    }
                                    if let Some(texture) = presented {
                                        if was_refining {
                                            this.gpu_pdf.surface_atomic_swaps = this
                                                .gpu_pdf
                                                .surface_atomic_swaps
                                                .saturating_add(1);
                                        }
                                        this.gpu_pdf.rendered.insert(
                                            page,
                                            RenderedGpuPage {
                                                key,
                                                request,
                                                refresh_region: surface_refresh_guard(
                                                    texture.region(),
                                                    request.visible_region,
                                                ),
                                                presentation,
                                                stats,
                                                latest_stats: stats,
                                                enqueued_at: completed_chain_started_at,
                                                first_paint_recorded: false,
                                                prewarmed: speculative
                                                    && !this.gpu_pdf.visible_pages.contains(&page),
                                                last_paint: None,
                                            },
                                        );
                                        notify = true;
                                    } else if incomplete {
                                        this.gpu_pdf.surface_incomplete_suppressed = this
                                            .gpu_pdf
                                            .surface_incomplete_suppressed
                                            .saturating_add(1);
                                        this.gpu_pdf.surface_refinement_submissions = this
                                            .gpu_pdf
                                            .surface_refinement_submissions
                                            .saturating_add(1);
                                        if let Some(rendered) =
                                            this.gpu_pdf.rendered.get_mut(&page)
                                        {
                                            rendered.presentation = presentation;
                                            rendered.latest_stats = stats;
                                        } else {
                                            this.gpu_pdf.rendered.insert(
                                                page,
                                                RenderedGpuPage {
                                                    key,
                                                    request,
                                                    refresh_region: region,
                                                    presentation,
                                                    stats,
                                                    latest_stats: stats,
                                                    enqueued_at: completed_chain_started_at,
                                                    first_paint_recorded: false,
                                                    prewarmed: speculative
                                                        && !this
                                                            .gpu_pdf
                                                            .visible_pages
                                                            .contains(&page),
                                                    last_paint: None,
                                                },
                                            );
                                        }
                                        continue_refinement = !newer_pending;
                                    } else {
                                        this.gpu_pdf.surface_render_discarded = this
                                            .gpu_pdf
                                            .surface_render_discarded
                                            .saturating_add(1);
                                    }
                                    this.gpu_pdf.surface_failures.remove(&page);
                                    this.gpu_pdf.prune_surface_cache();
                                }
                            }
                        }
                    }
                    Ok((presentation, GpuPageRenderOutcome::Deferred)) => {
                        this.gpu_pdf.surface_render_deferred =
                            this.gpu_pdf.surface_render_deferred.saturating_add(1);
                        // Retain a tighter safe region mode learned by the
                        // background planner without replacing any pixels.
                        if role == SurfaceRenderRole::Primary
                            && let Some(rendered) = this.gpu_pdf.rendered.get_mut(&page)
                        {
                            rendered.presentation = presentation;
                        }
                        this.gpu_pdf.surface_failures.remove(&page);
                    }
                    Ok((_, GpuPageRenderOutcome::Superseded)) => {
                        this.gpu_pdf.surface_render_superseded =
                            this.gpu_pdf.surface_render_superseded.saturating_add(1);
                    }
                    Err(error) => {
                        if this
                            .gpu_pdf
                            .renderer
                            .as_ref()
                            .is_some_and(|renderer| renderer.is_device_lost())
                        {
                            this.gpu_pdf.reset_device();
                            notify = true;
                        } else if !newer_pending && role == SurfaceRenderRole::Primary {
                            eprintln!(
                                "failed to seed native PDF page {} GPU surface: {error}",
                                page + 1
                            );
                            this.gpu_pdf.surface_failures.insert(page, error);
                            notify = true;
                        } else if role == SurfaceRenderRole::Continuity {
                            this.gpu_pdf.surface_render_discarded = this
                                .gpu_pdf
                                .surface_render_discarded
                                .saturating_add(1);
                        }
                    }
                }
                if continue_refinement {
                    this.enqueue_native_gpu_surface_render(
                        page,
                        key,
                        Arc::clone(&completion_page),
                        Arc::clone(&completion_renderer),
                        request,
                        region,
                        SurfaceRenderRole::Primary,
                        speculative,
                        SurfaceRefreshKind::RefinementContinuation,
                        Some(completed_chain_started_at),
                        cx,
                    );
                }
                this.dispatch_native_gpu_surface_render(cx);
                if notify {
                    cx.notify();
                }
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to install native PDF GPU surface seed: {error:#}");
            }
        });
        self.gpu_pdf.surface_seed_tasks.insert(
            page,
            SurfaceSeedTask {
                generation,
                key,
                region,
                request,
                role,
                speculative,
                superseded: false,
                enqueued_at,
                chain_started_at,
                started_at,
                in_flight_bytes,
                prepared,
                renderer,
                _task: task,
            },
        );
        self.gpu_pdf.surface_render_started = self.gpu_pdf.surface_render_started.saturating_add(1);
    }

    fn start_native_gpu_vector_lod_prepare(
        &mut self,
        page: u32,
        prepared: Arc<PreparedGpuPage>,
        renderer: Arc<GpuPdfRenderer>,
        required_scale: f32,
        cx: &mut Context<Self>,
    ) {
        if self
            .gpu_pdf
            .vector_lod_tasks
            .get(&page)
            .is_some_and(|task| same_patch_lod_request(task.required_scale, required_scale))
            || self
                .gpu_pdf
                .vector_lod_failures
                .get(&page)
                .is_some_and(|failure| {
                    same_patch_lod_request(failure.required_scale, required_scale)
                })
        {
            return;
        }
        if let Some(existing) = self.gpu_pdf.vector_lod_tasks.remove(&page) {
            existing.cancellation.cancel();
        }
        self.gpu_pdf.vector_lod_failures.remove(&page);
        self.gpu_pdf.surface_render_pending.remove(&page);
        if let Some(active) = self.gpu_pdf.surface_seed_tasks.get_mut(&page)
            && !active.superseded
        {
            self.gpu_pdf.render_generation = self.gpu_pdf.render_generation.wrapping_add(1).max(1);
            if active
                .renderer
                .supersede_page_render(&active.prepared, self.gpu_pdf.render_generation)
                .is_ok()
            {
                active.superseded = true;
            }
        }
        let cancellation = GpuCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker_page = Arc::clone(&prepared);
        let worker_renderer = Arc::clone(&renderer);
        let document_generation = self.gpu_pdf.generation;
        self.gpu_pdf.vector_lod_generation =
            self.gpu_pdf.vector_lod_generation.wrapping_add(1).max(1);
        let generation = self.gpu_pdf.vector_lod_generation;
        let token = renderer.token();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    worker_renderer
                        .prepare_vector_lod(&worker_page, required_scale, &worker_cancellation)
                        .map_err(|error| format!("{error:#}"))
                })
                .await;
            let update_result = this.update(cx, |this, cx| {
                if this.gpu_pdf.generation != document_generation
                    || !matches!(
                        this.gpu_pdf.vector_lod_tasks.get(&page),
                        Some(task) if task.generation == generation
                    )
                    || this
                        .gpu_pdf
                        .renderer
                        .as_ref()
                        .is_none_or(|renderer| renderer.token() != token)
                    || !matches!(
                        this.gpu_pdf.pages.get(&page),
                        Some(GpuPageState::Ready(current)) if Arc::ptr_eq(current, &prepared)
                    )
                {
                    return;
                }
                this.gpu_pdf.vector_lod_tasks.remove(&page);
                match result {
                    Ok(refined) => {
                        if let Some(task) = this.gpu_pdf.patch_lod_tasks.remove(&page) {
                            task.cancellation.cancel();
                        }
                        this.gpu_pdf
                            .pages
                            .insert(page, GpuPageState::Ready(refined));
                        this.gpu_pdf.vector_lod_failures.remove(&page);
                        this.gpu_pdf.surface_failures.remove(&page);
                        this.gpu_pdf
                            .prune_prepared_resource_cache(&this.gpu_pdf.visible_pages.clone());
                    }
                    Err(message) => {
                        eprintln!(
                            "failed to refine native PDF page {} vector LOD to {:.3}: {}",
                            page + 1,
                            required_scale,
                            message
                        );
                        this.gpu_pdf.vector_lod_failures.insert(
                            page,
                            VectorLodFailure {
                                required_scale,
                                message,
                            },
                        );
                    }
                }
                cx.notify();
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to install native PDF vector LOD: {error:#}");
            }
        });
        self.gpu_pdf.vector_lod_tasks.insert(
            page,
            VectorLodTask {
                generation,
                required_scale,
                cancellation,
                _task: task,
            },
        );
    }

    fn start_native_gpu_patch_lod_prepare(
        &mut self,
        page: u32,
        prepared: Arc<PreparedGpuPage>,
        renderer: Arc<GpuPdfRenderer>,
        request: GpuPaintRequest,
        required_scale: f32,
        cx: &mut Context<Self>,
    ) {
        if let Some(existing) = self.gpu_pdf.patch_lod_tasks.get(&page)
            && same_patch_lod_request(existing.required_scale, required_scale)
        {
            return;
        }
        if let Some(existing) = self.gpu_pdf.patch_lod_tasks.remove(&page) {
            existing.cancellation.cancel();
        }
        let cancellation = GpuCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker_page = Arc::clone(&prepared);
        let worker_renderer = Arc::clone(&renderer);
        let document_generation = self.gpu_pdf.generation;
        self.gpu_pdf.patch_lod_generation =
            self.gpu_pdf.patch_lod_generation.wrapping_add(1).max(1);
        let generation = self.gpu_pdf.patch_lod_generation;
        let token = renderer.token();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    worker_renderer
                        .prepare_patch_lods(&worker_page, request, &worker_cancellation)
                        .map_err(|error| format!("{error:#}"))
                })
                .await;
            let update_result = this.update(cx, |this, cx| {
                if this.gpu_pdf.generation != document_generation
                    || !matches!(
                        this.gpu_pdf.patch_lod_tasks.get(&page),
                        Some(task) if task.generation == generation
                    )
                    || this
                        .gpu_pdf
                        .renderer
                        .as_ref()
                        .is_none_or(|renderer| renderer.token() != token)
                    || !matches!(
                        this.gpu_pdf.pages.get(&page),
                        Some(GpuPageState::Ready(current)) if Arc::ptr_eq(current, &prepared)
                    )
                {
                    return;
                }
                this.gpu_pdf.patch_lod_tasks.remove(&page);
                match result {
                    Ok(_) => {
                        this.gpu_pdf.surface_failures.remove(&page);
                    }
                    Err(error) => {
                        eprintln!(
                            "failed to prepare native PDF page {} patch LOD: {error}",
                            page + 1
                        );
                        this.gpu_pdf.surface_failures.insert(page, error.clone());
                        this.gpu_pdf.pages.insert(page, GpuPageState::Failed(error));
                        this.gpu_pdf.rendered.remove(&page);
                        this.gpu_pdf.continuity_surfaces.remove(&page);
                    }
                }
                cx.notify();
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to install native PDF patch LOD: {error:#}");
            }
        });
        self.gpu_pdf.patch_lod_tasks.insert(
            page,
            PatchLodTask {
                generation,
                required_scale,
                cancellation,
                _task: task,
            },
        );
        cx.notify();
    }

    /// Resolves FitB-family target bounds off the UI thread, then executes the
    /// complete action tree in original order. A newer activation cancels and
    /// supersedes the older generation.
    pub(super) fn start_native_gpu_action_resolution(
        &mut self,
        action: GpuLinkAction,
        canonical_click: Option<[f64; 2]>,
        cx: &mut Context<Self>,
    ) {
        let Some(document) = self.gpu_pdf.document.clone() else {
            let diagnostic = GpuFallbackDiagnostic {
                feature: GpuFeatureId::Links,
                code: "application-destination-document-unavailable".into(),
                message: "the native PDF document is unavailable for destination resolution".into(),
                source: GpuFallbackSource::Application,
            };
            eprintln!(
                "native PDF destination fallback [{}:{}]: {}",
                diagnostic.feature, diagnostic.code, diagnostic.message
            );
            self.gpu_pdf.destination_fallback = Some(Arc::from([diagnostic]));
            cx.notify();
            return;
        };

        self.gpu_pdf.cancel_destination_task();
        self.gpu_pdf.destination_fallback = None;
        let cancellation = GpuCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let document_generation = self.gpu_pdf.generation;
        let destination_generation = self.gpu_pdf.destination_generation;
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    document.resolve_link_action(&action, &worker_cancellation)
                })
                .await;
            let update_result = this.update(cx, |this, cx| {
                if this.gpu_pdf.generation != document_generation
                    || this.gpu_pdf.destination_generation != destination_generation
                {
                    return;
                }
                this.gpu_pdf.destination_task = None;
                this.gpu_pdf.destination_cancellation = None;
                match result {
                    Ok(action) => {
                        this.gpu_pdf.destination_fallback = None;
                        this.activate_gpu_link_action(&action, canonical_click, cx);
                    }
                    Err(GpuActionResolutionError::Cancelled) => {}
                    Err(GpuActionResolutionError::Fallback(diagnostic)) => {
                        eprintln!(
                            "native PDF destination fallback [{}:{}]: {}",
                            diagnostic.feature, diagnostic.code, diagnostic.message
                        );
                        this.gpu_pdf.destination_fallback = Some(Arc::from([diagnostic]));
                        cx.notify();
                    }
                }
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to install native PDF destination: {error:#}");
            }
        });
        self.gpu_pdf.destination_cancellation = Some(cancellation);
        self.gpu_pdf.destination_task = Some(task);
        cx.notify();
    }

    pub(super) fn prune_native_gpu_pages(
        &mut self,
        visible_pages: &[usize],
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let visible = visible_pages
            .iter()
            .filter_map(|page| u32::try_from(*page).ok())
            .collect::<HashSet<_>>();
        let target_delta = self.target_scroll_y_doc - self.scroll_y_doc;
        let observed_delta = self
            .gpu_pdf
            .last_scroll_y_doc
            .map_or(0.0, |previous| self.scroll_y_doc - previous);
        self.gpu_pdf.last_scroll_y_doc = Some(self.scroll_y_doc);
        let direction = if target_delta.abs() > f32::EPSILON {
            target_delta
        } else {
            observed_delta
        };
        if direction.abs() > f32::EPSILON {
            self.gpu_pdf.last_scroll_direction = if direction < 0.0 { -1 } else { 1 };
        }
        let forward = self.gpu_pdf.last_scroll_direction >= 0;
        let page_count = u32::try_from(self.pages.len()).unwrap_or(u32::MAX);
        if self.native_gpu_preparation_motion_active() {
            let mut ordered_visible = visible.iter().copied().collect::<Vec<_>>();
            ordered_visible.sort_unstable();
            for page in ordered_visible {
                self.gpu_pdf
                    .motion_retained_pages
                    .retain(|retained| *retained != page);
                self.gpu_pdf.motion_retained_pages.push(page);
            }
            while self.gpu_pdf.motion_retained_pages.len() > MAX_RETAINED_GPU_PAGES {
                self.gpu_pdf.motion_retained_pages.remove(0);
            }
        } else {
            self.gpu_pdf.motion_retained_pages.clear();
        }
        let target_viewport = if self.is_presentation_mode() {
            visible.clone()
        } else {
            let scale = self.effective_scale().max(f32::EPSILON);
            let target_top = self.target_scroll_y_doc;
            let target_bottom = target_top + self.viewport_height / scale;
            let first = self.page_index_for_doc_y(target_top);
            let last = self
                .page_index_for_doc_y(target_bottom)
                .min(self.last_page_index());
            (first..=last)
                .filter_map(|page| u32::try_from(page).ok())
                .collect::<HashSet<_>>()
        };
        let (targets, _) = gpu_preparation_targets(
            &visible,
            &target_viewport,
            &self.gpu_pdf.motion_retained_pages,
            page_count,
            forward,
        );
        let retained = targets
            .iter()
            .map(|target| target.page)
            .collect::<HashSet<_>>();
        self.gpu_pdf.prune(&visible, &retained);
        let nearest_index = targets
            .iter()
            .filter(|target| target.speculative)
            .find(|target| {
                matches!(
                    self.gpu_pdf.pages.get(&target.page),
                    Some(GpuPageState::Ready(_))
                ) && !self.gpu_pdf.continuity_surfaces.contains_key(&target.page)
                    && !self.gpu_pdf.surface_seed_tasks.contains_key(&target.page)
                    && !self
                        .gpu_pdf
                        .surface_render_pending
                        .contains_key(&target.page)
            })
            .and_then(|target| usize::try_from(target.page).ok());

        let engine = cx
            .global::<SettingsStore>()
            .settings
            .document_view
            .pdf_render_engine;
        if !engine.uses_native_reader() || self.format != DocumentFormat::Pdf {
            return None;
        }
        self.gpu_pdf.set_preparation_demands(&targets);
        self.dispatch_native_gpu_page_preparations(cx);
        nearest_index
    }

    pub(super) fn warm_native_gpu_surface(
        &mut self,
        key: RenderKey,
        image_bounds: Bounds<Pixels>,
        viewport_bounds: Bounds<Pixels>,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.native_gpu_preparation_motion_active() {
            return;
        }
        let motion_active = self.native_gpu_preparation_motion_active();
        let all_visible_covered = !self.gpu_pdf.visible_pages.is_empty()
            && self
                .gpu_pdf
                .visible_pages
                .is_subset(&self.gpu_pdf.covered_pages);
        let visible_refinement_pending = self.gpu_pdf.visible_pages.iter().any(|page| {
            self.gpu_pdf
                .rendered
                .get(page)
                .is_some_and(|rendered| rendered.presentation.refinement_pending())
        });
        let visible_patch_lod_pending = self
            .gpu_pdf
            .patch_lod_tasks
            .keys()
            .any(|page| self.gpu_pdf.visible_pages.contains(page));
        let surface_pipeline_idle = self.gpu_pdf.surface_seed_tasks.is_empty()
            && self.gpu_pdf.surface_render_pending.is_empty();
        let Ok(page) = u32::try_from(key.page_idx) else {
            return;
        };
        let candidate_ready = matches!(self.gpu_pdf.pages.get(&page), Some(GpuPageState::Ready(_)));
        if self.gpu_pdf.continuity_surfaces.contains_key(&page)
            || self.gpu_pdf.surface_seed_tasks.contains_key(&page)
            || self.gpu_pdf.surface_render_pending.contains_key(&page)
        {
            return;
        }
        let predicted_viewport =
            adjacent_gpu_viewport_bounds(image_bounds, viewport_bounds, forward);
        if visible_gpu_region(key, image_bounds, predicted_viewport).is_none() {
            return;
        }
        let surface_region = full_page_gpu_region(continuity_render_key(key));
        let retained_surface_bytes = self.gpu_pdf.surface_bytes();
        let budget_fits = speculative_surface_fits(
            retained_surface_bytes,
            surface_region.byte_len(),
            MAX_RETAINED_GPU_SURFACE_BYTES,
        );
        if !should_warm_adjacent_surface(
            motion_active,
            all_visible_covered,
            visible_refinement_pending,
            visible_patch_lod_pending,
            surface_pipeline_idle,
            candidate_ready,
            budget_fits,
        ) {
            return;
        }

        _ = self.native_gpu_texture(
            key,
            image_bounds,
            predicted_viewport,
            GpuSurfaceIntent::AdjacentWarm,
            window,
            cx,
        );
    }

    pub(super) fn native_gpu_fallback_diagnostics(
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

    pub(super) fn native_gpu_warning_diagnostics(
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

    pub(super) fn native_gpu_navigation_fallback_diagnostics(
        &self,
    ) -> Option<&[GpuFallbackDiagnostic]> {
        self.gpu_pdf
            .destination_fallback
            .as_deref()
            .or(self.gpu_pdf.navigation_fallback.as_deref())
    }

    pub(super) fn native_gpu_link_interactions(
        &self,
        page: usize,
    ) -> Option<&[GpuLinkInteraction]> {
        let page = u32::try_from(page).ok()?;
        if !self.gpu_pdf.presented_pages.contains(&page) {
            return None;
        }
        match self.gpu_pdf.pages.get(&page)? {
            GpuPageState::Ready(prepared) => Some(prepared.link_interactions()),
            _ => None,
        }
    }

    pub(super) fn cached_native_gpu_link_mapping(&self, page: usize) -> Option<&[(usize, usize)]> {
        let page = u32::try_from(page).ok()?;
        match self.gpu_pdf.link_mappings.get(&page)? {
            CachedLinkMapping::Valid(mapping) => Some(mapping),
            CachedLinkMapping::Invalid => None,
        }
    }

    pub(super) fn native_gpu_render(&self, page: usize) -> Option<(RenderKey, GpuRenderStats)> {
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

    pub(super) fn native_gpu_preparation_stats(&self, page: usize) -> Option<GpuPreparationStats> {
        let page = u32::try_from(page).ok()?;
        match self.gpu_pdf.pages.get(&page)? {
            GpuPageState::Ready(page) => Some(page.preparation_stats()),
            _ => None,
        }
    }

    pub(super) fn native_gpu_preparation_timings(
        &self,
        page: usize,
    ) -> Option<GpuPagePreparationTimings> {
        let page = u32::try_from(page).ok()?;
        match self.gpu_pdf.pages.get(&page)? {
            GpuPageState::Ready(page) => Some(page.preparation_timings()),
            _ => None,
        }
    }

    pub(super) fn native_gpu_patch_lod_preparation_stats(
        &self,
        page: usize,
    ) -> Option<GpuPatchLodPreparationStats> {
        let page = u32::try_from(page).ok()?;
        match self.gpu_pdf.pages.get(&page)? {
            GpuPageState::Ready(page) => Some(page.patch_lod_preparation_stats()),
            _ => None,
        }
    }

    pub(super) fn native_gpu_cache_debug(&self) -> GpuCacheDebugSnapshot {
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
            surface_seed_tasks: self
                .gpu_pdf
                .surface_seed_tasks
                .len()
                .saturating_add(self.gpu_pdf.motion_render_tasks.len())
                .saturating_add(
                    self.gpu_pdf
                        .motion_render_queued
                        .values()
                        .map(VecDeque::len)
                        .sum::<usize>(),
                ),
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
            surface_zoom_live_redraws: self.gpu_pdf.surface_zoom_live_redraws,
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
            motion_render_jobs_started: self.gpu_pdf.motion_render_started,
            motion_render_jobs_completed: self.gpu_pdf.motion_render_completed,
            motion_render_jobs_superseded: self.gpu_pdf.motion_render_superseded,
            motion_render_jobs_pending: self.gpu_pdf.motion_render_tasks.len().saturating_add(
                self.gpu_pdf
                    .motion_render_queued
                    .values()
                    .map(VecDeque::len)
                    .sum::<usize>(),
            ),
            motion_render_jobs_peak: self.gpu_pdf.motion_render_peak,
            motion_render_detached_target_reuses: self.gpu_pdf.motion_render_detached_reuses,
            motion_target_reservations_started: self.gpu_pdf.motion_reservations_started,
            motion_target_reservations_consumed: self.gpu_pdf.motion_reservations_consumed,
            motion_target_reservations_released: self.gpu_pdf.motion_reservations_released,
            motion_target_reservations_pending: self
                .gpu_pdf
                .motion_target_reservations
                .len()
                .saturating_add(
                    self.gpu_pdf
                        .motion_render_queued
                        .values()
                        .map(VecDeque::len)
                        .sum::<usize>(),
                ),
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

    pub(super) fn native_gpu_failure(&self, page: usize) -> Option<&str> {
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

    pub(super) fn native_gpu_vector_lod_failure(&self, page: usize) -> Option<&str> {
        let page = u32::try_from(page).ok()?;
        self.gpu_pdf
            .vector_lod_failures
            .get(&page)
            .map(|failure| failure.message.as_str())
    }

    pub(super) fn native_gpu_pending(&self, page: usize) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn render_key() -> RenderKey {
        RenderKey {
            doc_id: 1,
            page_idx: 0,
            source_revision: 0,
            rotation: DocumentRotation::Degrees0,
            color_transform: None,
            physical_width: 1_000,
            physical_height: 2_000,
        }
    }

    #[test]
    fn vector_refinement_advances_in_bounded_stages_to_the_reader_limit() {
        assert_eq!(next_vector_lod_scale(8.0, 11.264), Some(11.264));
        assert_eq!(next_vector_lod_scale(8.0, 8_192.0), Some(16.0));
        assert_eq!(next_vector_lod_scale(4_096.0, 8_192.0), Some(8_192.0));
        assert_eq!(next_vector_lod_scale(8.0, 8.0), None);
        assert_eq!(next_vector_lod_scale(8_192.0, 16_384.0), Some(16_384.0));
    }

    #[test]
    fn motion_queue_keeps_only_the_latest_camera_request() {
        let mut queued = VecDeque::from([1_u32, 2]);
        assert_eq!(replace_queued_motion(&mut queued, 3), 2);
        assert_eq!(queued, VecDeque::from([3]));
    }

    #[test]
    fn offscreen_retained_refinements_do_not_keep_the_visible_pipeline_pending() {
        let presented = HashSet::from([2, 3]);
        assert_eq!(presented_refinement_page_count(&presented, [1, 2, 4]), 1);
        assert_eq!(presented_refinement_page_count(&presented, [1, 4]), 0);
    }

    #[test]
    fn continuity_key_is_stable_aspect_preserving_and_bounded() {
        let exact = render_key();
        let continuity = continuity_render_key(exact);
        assert_eq!(continuity.physical_width, exact.physical_width);
        assert_eq!(continuity.physical_height, exact.physical_height);
        assert_eq!(continuity.doc_id, exact.doc_id);
        assert_eq!(continuity.page_idx, exact.page_idx);
        assert!(continuity.byte_len() <= 16 * 1024 * 1024);

        let landscape = RenderKey {
            physical_width: 4_000,
            physical_height: 1_000,
            ..exact
        };
        let landscape_continuity = continuity_render_key(landscape);
        assert_eq!(
            landscape_continuity.physical_width,
            GPU_CONTINUITY_LONG_EDGE
        );
        assert_eq!(landscape_continuity.physical_height, 512);

        let small = RenderKey {
            physical_width: 640,
            physical_height: 960,
            ..exact
        };
        assert_eq!(
            continuity_render_key(small),
            small,
            "continuity rendering must never upscale beyond the exact display key"
        );
    }

    #[test]
    fn viewport_crop_keeps_device_density_above_the_raster_dimension_limit() {
        let key = RenderKey {
            physical_width: 40_000,
            physical_height: 20_000,
            ..render_key()
        };
        let image = Bounds::new(point(px(0.0), px(0.0)), size(px(20_000.0), px(10_000.0)));
        let viewport = Bounds::new(
            point(px(10_000.0), px(5_000.0)),
            size(px(1_000.0), px(800.0)),
        );
        let region = visible_gpu_region(key, image, viewport).expect("visible virtual crop");
        assert_eq!(
            region,
            GpuPageRegion {
                x: 20_000,
                y: 10_000,
                width: 2_000,
                height: 1_600,
            }
        );
        assert_eq!(
            physical_gpu_viewport_size(key, image, viewport),
            Some([2_000, 1_600])
        );
        assert_eq!(gpu_surface_bounds(key, image, region), viewport);

        let capped = RenderKey {
            physical_width: 8_192,
            physical_height: 4_096,
            ..key
        };
        assert_eq!(
            physical_gpu_viewport_size(capped, image, viewport),
            Some([410, 328]),
            "the former raster-sized page space undersampled the visible viewport"
        );
    }

    #[test]
    fn continuity_prevents_motion_selection_gaps() {
        assert_eq!(
            presented_surface_kind(true, true, true),
            Some(PresentedSurfaceKind::Primary)
        );
        assert_eq!(
            presented_surface_kind(false, true, true),
            Some(PresentedSurfaceKind::Continuity)
        );
        assert_eq!(
            presented_surface_kind(false, false, true),
            Some(PresentedSurfaceKind::PartialPrimary)
        );
        assert_eq!(presented_surface_kind(false, false, false), None);

        let primary = GpuPageRegion {
            x: 200,
            y: 300,
            width: 600,
            height: 800,
        };
        for frame in 0..120_u32 {
            let visible = GpuPageRegion {
                x: frame.saturating_mul(10),
                y: frame.saturating_mul(15),
                width: 400,
                height: 500,
            };
            let primary_covers = gpu_region_contains(primary, visible);
            let primary_overlaps = gpu_regions_overlap(primary, visible);
            assert!(
                presented_surface_kind(primary_covers, true, primary_overlaps).is_some(),
                "frame {frame} lost all presentation coverage"
            );
        }
    }

    #[test]
    fn motion_density_gate_is_bounded_and_source_aware() {
        let requested = render_key();
        let mut ninety_percent = requested;
        ninety_percent.physical_width = 900;
        ninety_percent.physical_height = 1_800;
        assert!(motion_surface_density_sufficient(ninety_percent, requested));

        let mut below_threshold = ninety_percent;
        below_threshold.physical_width -= 1;
        assert!(!motion_surface_density_sufficient(
            below_threshold,
            requested
        ));

        let mut higher_resolution = requested;
        higher_resolution.physical_width = 2_000;
        higher_resolution.physical_height = 4_000;
        assert!(motion_surface_density_sufficient(
            higher_resolution,
            requested
        ));

        let mut different_source = requested;
        different_source.source_revision = 1;
        assert!(!motion_surface_density_sufficient(
            requested,
            different_source
        ));
    }

    #[test]
    fn fixed_scale_scroll_defers_refresh_only_with_a_full_page_safety_net() {
        assert!(should_defer_fixed_scale_scroll_refresh(
            false, true, true, true,
        ));
        assert!(!should_defer_fixed_scale_scroll_refresh(
            true, true, true, true,
        ));
        assert!(!should_defer_fixed_scale_scroll_refresh(
            false, false, true, true,
        ));
        assert!(!should_defer_fixed_scale_scroll_refresh(
            false, true, false, true,
        ));
        assert!(!should_defer_fixed_scale_scroll_refresh(
            false, true, true, false,
        ));
    }

    #[test]
    fn continuity_never_bypasses_an_active_scale_redraw() {
        let continuity = Some(PresentedSurfaceKind::Continuity);
        assert!(should_present_continuity_motion(
            false, true, true, continuity,
        ));
        assert!(should_present_continuity_motion(
            false, false, false, continuity,
        ));
        assert!(!should_present_continuity_motion(
            false, false, true, continuity,
        ));
        assert!(!should_present_continuity_motion(
            true, true, true, continuity,
        ));
        assert!(!should_present_continuity_motion(
            false,
            true,
            true,
            Some(PresentedSurfaceKind::Primary),
        ));
    }

    #[test]
    fn zoom_motion_cancels_only_replaceable_or_lower_priority_work() {
        assert!(zoom_motion_cancels_surface_work(
            true,
            SurfaceRenderRole::Primary,
            false,
        ));
        assert!(zoom_motion_cancels_surface_work(
            false,
            SurfaceRenderRole::Continuity,
            false,
        ));
        assert!(zoom_motion_cancels_surface_work(
            false,
            SurfaceRenderRole::Primary,
            true,
        ));
        assert!(!zoom_motion_cancels_surface_work(
            false,
            SurfaceRenderRole::Primary,
            false,
        ));
    }

    #[test]
    fn patch_lod_worker_is_reused_only_for_the_same_declared_scale_slot() {
        assert!(same_patch_lod_request(4.0, 4.0));
        assert!(!same_patch_lod_request(4.0, 8.0));
        assert!(!same_patch_lod_request(8.0, 4.0));
    }

    #[test]
    fn preparation_window_is_directional_and_deterministic() {
        let visible = [4, 5].into_iter().collect::<HashSet<_>>();
        let (forward, nearest_forward) =
            gpu_preparation_targets(&visible, &HashSet::new(), &[], 10, true);
        assert_eq!(nearest_forward, Some(6));
        assert_eq!(
            forward
                .iter()
                .map(|target| (target.page, target.speculative))
                .collect::<Vec<_>>(),
            vec![
                (4, false),
                (5, false),
                (6, true),
                (7, true),
                (8, true),
                (9, true),
                (3, true),
                (2, true)
            ]
        );

        let (backward, nearest_backward) =
            gpu_preparation_targets(&visible, &HashSet::new(), &[], 10, false);
        assert_eq!(nearest_backward, Some(3));
        assert_eq!(
            backward
                .iter()
                .map(|target| (target.page, target.speculative))
                .collect::<Vec<_>>(),
            vec![
                (4, false),
                (5, false),
                (3, true),
                (2, true),
                (1, true),
                (0, true),
                (6, true),
                (7, true)
            ]
        );
    }

    #[test]
    fn target_viewport_precedes_the_directional_scroll_corridor() {
        let visible = [2].into_iter().collect::<HashSet<_>>();
        let target = [6, 7].into_iter().collect::<HashSet<_>>();
        let (targets, nearest) = gpu_preparation_targets(&visible, &target, &[], 12, true);
        assert_eq!(nearest, Some(3));
        assert_eq!(
            targets
                .iter()
                .map(|target| (target.page, target.speculative))
                .collect::<Vec<_>>(),
            vec![
                (2, false),
                (6, false),
                (7, false),
                (3, true),
                (4, true),
                (5, true),
                (8, true),
                (1, true),
            ]
        );
    }

    #[test]
    fn recent_motion_history_precedes_directional_speculation() {
        let visible = HashSet::from([6]);
        let history = [2, 3, 4, 5, 6];
        let (targets, nearest) =
            gpu_preparation_targets(&visible, &HashSet::new(), &history, 12, true);
        assert_eq!(nearest, Some(7));
        assert_eq!(
            targets
                .iter()
                .map(|target| (target.page, target.speculative))
                .collect::<Vec<_>>(),
            vec![
                (6, false),
                (5, true),
                (4, true),
                (3, true),
                (2, true),
                (7, true),
                (8, true),
                (9, true),
            ]
        );
    }

    #[test]
    fn visible_pages_consume_the_speculative_cache_budget_first() {
        let visible = (0..MAX_RETAINED_GPU_PAGES as u32).collect::<HashSet<_>>();
        let (targets, nearest) = gpu_preparation_targets(&visible, &HashSet::new(), &[], 20, true);
        assert_eq!(targets.len(), visible.len());
        assert!(targets.iter().all(|target| !target.speculative));
        assert_eq!(nearest, None);
    }

    #[test]
    fn zoom_motion_suspends_speculative_preparation() {
        assert!(!speculative_preparation_allowed(true, true, 0));
        assert!(!speculative_preparation_allowed(true, true, 1));
        assert!(speculative_preparation_allowed(true, false, 0));
        assert!(!speculative_preparation_allowed(true, false, 1));
        assert!(speculative_preparation_allowed(false, false, 1));
    }

    #[test]
    fn demand_changes_update_active_priority_without_restarting_the_page() {
        let mut state = GpuPdfState::default();
        let cancellation = GpuCancellationToken::new();
        state.pages.insert(
            4,
            GpuPageState::Preparing {
                generation: 1,
                cancellation: cancellation.clone(),
                demand: GpuPreparationDemand {
                    priority: 4,
                    speculative: true,
                    sequence: 1,
                },
                _task: Task::ready(()),
            },
        );
        state.preparation_demands.insert(
            4,
            GpuPreparationDemand {
                priority: 4,
                speculative: true,
                sequence: 1,
            },
        );
        state.set_preparation_demands(&[GpuPreparationTarget {
            page: 4,
            priority: 0,
            speculative: false,
        }]);

        assert!(!cancellation.is_cancelled());
        assert!(matches!(
            state.pages.get(&4),
            Some(GpuPageState::Preparing {
                demand: GpuPreparationDemand {
                    priority: 0,
                    speculative: false,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn pruning_cancels_stale_preparation_and_drops_its_demand() {
        let mut state = GpuPdfState::default();
        let cancellation = GpuCancellationToken::new();
        let demand = GpuPreparationDemand {
            priority: 2,
            speculative: true,
            sequence: 1,
        };
        state.pages.insert(
            7,
            GpuPageState::Preparing {
                generation: 1,
                cancellation: cancellation.clone(),
                demand,
                _task: Task::ready(()),
            },
        );
        state.preparation_demands.insert(7, demand);
        state.prune(&HashSet::new(), &HashSet::new());

        assert!(cancellation.is_cancelled());
        assert!(!state.pages.contains_key(&7));
        assert!(!state.preparation_demands.contains_key(&7));
        assert_eq!(state.preparation_cancelled, 1);
    }

    #[test]
    fn visible_preemption_selects_the_least_useful_speculative_worker() {
        let mut state = GpuPdfState::default();
        for (page, priority, sequence) in [(6, 2, 1), (8, 4, 2)] {
            let demand = GpuPreparationDemand {
                priority,
                speculative: true,
                sequence,
            };
            state.preparation_demands.insert(page, demand);
            state.pages.insert(
                page,
                GpuPageState::Preparing {
                    generation: sequence,
                    cancellation: GpuCancellationToken::new(),
                    demand,
                    _task: Task::ready(()),
                },
            );
        }

        assert_eq!(state.worst_speculative_preparation(), Some(8));
        state.cancel_active_preparation(8, true);
        assert!(!state.pages.contains_key(&8));
        assert!(state.preparation_demands.contains_key(&8));
        assert_eq!(state.preparation_cancelled, 1);
    }

    #[test]
    fn terminal_pages_are_not_requeued_until_state_reset() {
        let mut state = GpuPdfState::default();
        state.terminal_pages.insert(3);
        state.set_preparation_demands(&[GpuPreparationTarget {
            page: 3,
            priority: 0,
            speculative: false,
        }]);
        assert!(!state.preparation_demands.contains_key(&3));

        state.reset();
        state.demand_page(3, false, 0);
        assert!(state.preparation_demands.contains_key(&3));
    }

    #[test]
    fn viewport_intersection_maps_outward_to_page_pixels() {
        let image = Bounds::new(point(px(100.0), px(50.0)), size(px(500.0), px(1_000.0)));
        let viewport = Bounds::new(point(px(200.0), px(300.0)), size(px(300.0), px(400.0)));
        assert_eq!(
            visible_gpu_region(render_key(), image, viewport),
            Some(GpuPageRegion {
                x: 200,
                y: 500,
                width: 600,
                height: 800,
            })
        );
    }

    #[test]
    fn clipped_surface_maps_back_to_its_page_destination() {
        let image = Bounds::new(point(px(100.0), px(50.0)), size(px(500.0), px(1_000.0)));
        let bounds = gpu_surface_bounds(
            render_key(),
            image,
            GpuPageRegion {
                x: 200,
                y: 500,
                width: 600,
                height: 800,
            },
        );
        assert_eq!(bounds.origin, point(px(200.0), px(300.0)));
        assert_eq!(bounds.size, size(px(300.0), px(400.0)));
    }

    #[test]
    fn adjacent_page_warming_predicts_the_entering_viewport_edge() {
        let image = Bounds::new(point(px(100.0), px(100.0)), size(px(500.0), px(1_000.0)));
        let viewport = Bounds::new(point(px(0.0), px(0.0)), size(px(800.0), px(600.0)));

        let forward = adjacent_gpu_viewport_bounds(image, viewport, true);
        assert_eq!(forward.origin, point(px(0.0), px(100.0)));
        assert_eq!(
            visible_gpu_region(render_key(), image, forward),
            Some(GpuPageRegion {
                x: 0,
                y: 0,
                width: 1_000,
                height: 1_200,
            })
        );

        let backward = adjacent_gpu_viewport_bounds(image, viewport, false);
        assert_eq!(backward.origin, point(px(0.0), px(500.0)));
        assert_eq!(
            visible_gpu_region(render_key(), image, backward),
            Some(GpuPageRegion {
                x: 0,
                y: 800,
                width: 1_000,
                height: 1_200,
            })
        );
    }

    #[test]
    fn adjacent_surface_intent_never_demands_preparation_or_starts_fallback_grace() {
        assert!(should_demand_missing_gpu_page(GpuSurfaceIntent::Visible));
        assert!(!should_demand_missing_gpu_page(
            GpuSurfaceIntent::AdjacentWarm
        ));
        assert!(should_start_gpu_fallback_grace(
            PdfRenderEngine::GlyphstFirst,
            GpuSurfaceIntent::Visible,
        ));
        assert!(!should_start_gpu_fallback_grace(
            PdfRenderEngine::GlyphstFirst,
            GpuSurfaceIntent::AdjacentWarm,
        ));
        assert!(!should_start_gpu_fallback_grace(
            PdfRenderEngine::Glyphst,
            GpuSurfaceIntent::Visible,
        ));
    }

    #[test]
    fn adjacent_surface_warming_waits_for_visible_coverage_and_higher_priority_work() {
        assert!(should_warm_adjacent_surface(
            true, true, true, false, true, true, true,
        ));
        assert!(should_warm_adjacent_surface(
            false, true, false, false, true, true, true,
        ));
        assert!(!should_warm_adjacent_surface(
            false, true, true, false, true, true, true,
        ));
        assert!(!should_warm_adjacent_surface(
            true, false, false, false, true, true, true,
        ));
        assert!(!should_warm_adjacent_surface(
            true, true, false, true, true, true, true,
        ));
        assert!(!should_warm_adjacent_surface(
            true, true, false, false, false, true, true,
        ));
        assert!(!should_warm_adjacent_surface(
            true, true, false, false, true, false, true,
        ));
        assert!(!should_warm_adjacent_surface(
            true, true, false, false, true, true, false,
        ));
    }

    #[test]
    fn adjacent_warming_uses_motion_quality_then_exact_settled_quality() {
        let key = render_key();
        let visible_region = GpuPageRegion {
            x: 0,
            y: 0,
            width: 1_000,
            height: 1_000,
        };
        assert!(!gpu_paint_request(key, visible_region, false, None, None).settled);
        assert!(gpu_paint_request(key, visible_region, true, None, None).settled);
    }

    #[test]
    fn stable_target_capacity_is_invariant_under_reader_zoom() {
        let key = render_key();
        let image = Bounds::new(point(px(0.0), px(0.0)), size(px(500.0), px(1_000.0)));
        let viewport = Bounds::new(point(px(0.0), px(0.0)), size(px(800.0), px(600.0)));
        let capacity = stable_gpu_target_capacity(key, image, viewport, 8_192).unwrap();
        assert_eq!(capacity, [2_432, 1_920]);
        let motion_capacity =
            stable_gpu_motion_target_capacity(key, image, viewport, 8_192).unwrap();
        assert_eq!(motion_capacity, [1_664, 1_280]);

        let zoomed_key = RenderKey {
            physical_width: 2_000,
            physical_height: 4_000,
            ..key
        };
        let zoomed_image = Bounds::new(point(px(0.0), px(0.0)), size(px(1_000.0), px(2_000.0)));
        assert_eq!(
            stable_gpu_target_capacity(zoomed_key, zoomed_image, viewport, 8_192),
            Some(capacity)
        );
        assert_eq!(
            stable_gpu_motion_target_capacity(zoomed_key, zoomed_image, viewport, 8_192),
            Some(motion_capacity)
        );
    }

    #[test]
    fn speculative_surface_budget_and_cache_pressure_never_poison_visible_routing() {
        assert!(speculative_surface_fits(100, 25, 125));
        assert!(!speculative_surface_fits(100, 26, 125));
        assert!(!speculative_surface_fits(u64::MAX, 1, u64::MAX));

        assert_eq!(
            surface_cache_pressure_action(true, false),
            SurfaceCachePressureAction::PreserveDiagnostics,
        );
        assert_eq!(
            surface_cache_pressure_action(true, true),
            SurfaceCachePressureAction::ClearDiagnostics,
        );
        assert_eq!(
            surface_cache_pressure_action(false, true),
            SurfaceCachePressureAction::ClearDiagnostics,
        );
        assert_eq!(
            surface_cache_pressure_action(false, false),
            SurfaceCachePressureAction::RouteToFallback,
        );
    }

    #[test]
    fn prewarmed_hit_requires_compatible_scale_content_and_coverage() {
        let key = render_key();
        let request = gpu_paint_request(
            key,
            GpuPageRegion {
                x: 100,
                y: 400,
                width: 800,
                height: 800,
            },
            false,
            None,
            None,
        );
        let surface = request.surface_region().expect("warm surface region");
        assert!(prewarmed_surface_covers(
            true,
            key,
            surface,
            key,
            request.visible_region,
            true,
        ));
        assert!(!prewarmed_surface_covers(
            false,
            key,
            surface,
            key,
            request.visible_region,
            true,
        ));
        assert!(!prewarmed_surface_covers(
            true,
            key,
            surface,
            key,
            request.visible_region,
            false,
        ));

        let compatible_motion_key = RenderKey {
            physical_width: 1_100,
            physical_height: 2_200,
            ..key
        };
        assert!(prewarmed_surface_covers(
            true,
            key,
            surface,
            compatible_motion_key,
            request.visible_region,
            true,
        ));

        let mut scaled_key = key;
        scaled_key.physical_width = 2_000;
        assert!(!prewarmed_surface_covers(
            true,
            key,
            surface,
            scaled_key,
            request.visible_region,
            true,
        ));
        assert!(!prewarmed_surface_covers(
            true,
            key,
            GpuPageRegion {
                height: 128,
                ..surface
            },
            key,
            request.visible_region,
            true,
        ));
    }

    #[test]
    fn visible_surface_request_supersedes_matching_speculative_work() {
        assert!(!can_reuse_active_surface_request(true, true, false));
        assert!(can_reuse_active_surface_request(true, true, true));
        assert!(can_reuse_active_surface_request(true, false, false));
        assert!(can_reuse_active_surface_request(true, false, true));
        assert!(!can_reuse_active_surface_request(false, false, false));
    }

    #[test]
    fn sequential_surface_warming_keeps_exactly_one_page_of_lookahead() {
        let page_count = 8_u32;
        let mut prewarmed = None;
        let mut hits = 0_u32;
        let mut maximum_resident = 0_usize;

        for visible in 0..page_count {
            if prewarmed == Some(visible) {
                hits += 1;
                prewarmed = None;
            }
            if visible + 1 < page_count
                && should_warm_adjacent_surface(true, true, true, false, true, true, true)
            {
                assert!(prewarmed.is_none());
                prewarmed = Some(visible + 1);
            }
            maximum_resident = maximum_resident.max(usize::from(prewarmed.is_some()));
        }

        assert_eq!(hits, page_count - 1);
        assert_eq!(maximum_resident, 1);
    }

    #[test]
    fn cold_visible_pages_are_not_starved_during_motion() {
        assert!(should_start_surface_seed(false, false, false));
        assert!(!should_start_surface_seed(false, false, true));
        assert!(should_start_surface_seed(false, true, true));
        assert!(should_start_surface_seed(true, false, true));

        assert!(!should_defer_full_surface_render(false, false, false));
        assert!(should_defer_full_surface_render(false, false, true));
        assert!(!should_defer_full_surface_render(false, true, true));
        assert!(!should_defer_full_surface_render(true, false, true));
    }

    #[test]
    fn surface_containment_is_edge_inclusive_and_overflow_safe() {
        let surface = GpuPageRegion {
            x: 100,
            y: 200,
            width: 400,
            height: 600,
        };
        assert!(gpu_region_contains(surface, surface));
        assert!(gpu_region_contains(
            surface,
            GpuPageRegion {
                x: 150,
                y: 250,
                width: 300,
                height: 500,
            }
        ));
        assert!(!gpu_region_contains(
            surface,
            GpuPageRegion { x: 99, ..surface }
        ));
        assert!(!gpu_region_contains(
            surface,
            GpuPageRegion {
                x: 450,
                width: 51,
                ..surface
            }
        ));

        let overflow_edge = GpuPageRegion {
            x: u32::MAX - 50,
            y: u32::MAX - 50,
            width: 100,
            height: 100,
        };
        assert!(gpu_region_contains(
            overflow_edge,
            GpuPageRegion {
                x: u32::MAX - 40,
                y: u32::MAX - 40,
                width: 20,
                height: 20,
            }
        ));
    }

    #[test]
    fn surface_refresh_guard_uses_half_of_each_available_margin() {
        let surface = GpuPageRegion {
            x: 100,
            y: 200,
            width: 1_000,
            height: 800,
        };
        let visible = GpuPageRegion {
            x: 300,
            y: 300,
            width: 400,
            height: 400,
        };
        assert_eq!(
            surface_refresh_guard(surface, visible),
            GpuPageRegion {
                x: 200,
                y: 250,
                width: 700,
                height: 600,
            }
        );

        let page_edge_surface = GpuPageRegion {
            x: 0,
            y: 0,
            width: 800,
            height: 900,
        };
        let page_edge_visible = GpuPageRegion {
            x: 0,
            y: 100,
            width: 500,
            height: 500,
        };
        assert_eq!(
            surface_refresh_guard(page_edge_surface, page_edge_visible),
            GpuPageRegion {
                x: 0,
                y: 50,
                width: 650,
                height: 700,
            },
            "page edges have no artificial margin to inset"
        );
        assert_eq!(
            surface_refresh_guard(visible, visible),
            visible,
            "a budget-tight visible-only surface has a zero-width gutter"
        );

        let overflow_surface = GpuPageRegion {
            x: u32::MAX - 100,
            y: u32::MAX - 100,
            width: 100,
            height: 100,
        };
        let overflow_visible = GpuPageRegion {
            x: u32::MAX - 80,
            y: u32::MAX - 60,
            width: 40,
            height: 20,
        };
        assert_eq!(
            surface_refresh_guard(overflow_surface, overflow_visible),
            GpuPageRegion {
                x: u32::MAX - 90,
                y: u32::MAX - 80,
                width: 70,
                height: 60,
            }
        );
    }

    #[test]
    fn overscan_refresh_trajectory_stays_covered_with_few_submissions() {
        let mut request = GpuPaintRequest {
            page_size: [1_000, 6_000],
            visible_region: GpuPageRegion {
                x: 0,
                y: 500,
                width: 1_000,
                height: 800,
            },
            target_capacity: None,
            motion_target_capacity: None,
            rotation: DocumentRotation::Degrees0,
            background: [1.0, 1.0, 1.0, 1.0],
            settled: false,
            generation: 1,
        };
        let mut surface = request.surface_region().expect("initial surface region");
        let mut guard = surface_refresh_guard(surface, request.visible_region);
        let mut submissions = 1_u32;
        let mut uncovered_frames = 0_u32;

        for frame in 1..=120_u32 {
            request.visible_region.y = 500 + frame * 8;
            if !gpu_region_contains(surface, request.visible_region) {
                uncovered_frames += 1;
            }
            if !gpu_region_contains(guard, request.visible_region) {
                let next_surface = request.surface_region().expect("shifted surface region");
                if next_surface != surface {
                    surface = next_surface;
                    guard = surface_refresh_guard(surface, request.visible_region);
                    submissions += 1;
                }
            }
        }

        assert_eq!(uncovered_frames, 0);
        assert!(
            submissions * 4 < 120,
            "{submissions} submissions should remain below 25% of paint frames"
        );
    }

    #[test]
    fn surface_request_coalescing_tracks_quality_and_visible_coverage() {
        let request = GpuPaintRequest {
            page_size: [1_000, 2_000],
            visible_region: GpuPageRegion {
                x: 10,
                y: 20,
                width: 800,
                height: 1_200,
            },
            target_capacity: None,
            motion_target_capacity: None,
            rotation: DocumentRotation::Degrees0,
            background: [1.0, 1.0, 1.0, 1.0],
            settled: false,
            generation: 1,
        };
        let region = request.surface_region().expect("surface region");
        let shifted_request = GpuPaintRequest {
            visible_region: GpuPageRegion {
                y: 21,
                ..request.visible_region
            },
            generation: 2,
            ..request
        };
        assert!(same_gpu_surface_content(
            request,
            GpuPaintRequest {
                generation: 2,
                ..request
            }
        ));
        assert!(same_gpu_surface_content(request, shifted_request));
        assert!(same_gpu_surface_work(request, shifted_request));

        let key = render_key();
        let identity = SurfaceRequestIdentity::new(key, request, region);
        let shifted_identity = SurfaceRequestIdentity::new(key, shifted_request, region);
        assert!(surface_request_covers(identity, shifted_identity));
        assert!(can_coalesce_pending_surface_request(
            identity,
            false,
            shifted_identity,
            false,
        ));
        assert!(!can_coalesce_pending_surface_request(
            identity, true, identity, false,
        ));
        assert!(surface_request_covers(
            identity,
            SurfaceRequestIdentity::new(
                key,
                shifted_request,
                GpuPageRegion {
                    y: region.y + 128,
                    ..region
                },
            ),
        ));
        assert!(!surface_request_covers(
            identity,
            SurfaceRequestIdentity::new(
                key,
                GpuPaintRequest {
                    settled: true,
                    ..request
                },
                region,
            ),
        ));
        assert!(!same_gpu_surface_content(
            request,
            GpuPaintRequest {
                background: [0.0, 0.0, 0.0, 1.0],
                ..request
            }
        ));
        assert!(!same_gpu_surface_content(
            request,
            GpuPaintRequest {
                rotation: DocumentRotation::Degrees90,
                ..request
            }
        ));

        let mut scaled_key = key;
        scaled_key.physical_width = 2_000;
        assert!(!surface_request_covers(
            identity,
            SurfaceRequestIdentity::new(
                scaled_key,
                GpuPaintRequest {
                    page_size: [2_000, 2_000],
                    ..request
                },
                region,
            ),
        ));

        let outside_request = GpuPaintRequest {
            visible_region: GpuPageRegion {
                x: region.right(),
                y: region.y,
                width: 1,
                height: 1,
            },
            ..request
        };
        assert!(!surface_request_covers(
            identity,
            SurfaceRequestIdentity::new(key, outside_request, region),
        ));
    }

    #[test]
    fn material_surface_work_receives_the_next_nonzero_generation() {
        let mut request = GpuPaintRequest {
            page_size: [1_000, 2_000],
            visible_region: GpuPageRegion {
                x: 0,
                y: 0,
                width: 1_000,
                height: 1_000,
            },
            target_capacity: None,
            motion_target_capacity: None,
            rotation: DocumentRotation::Degrees0,
            background: [1.0, 1.0, 1.0, 1.0],
            settled: false,
            generation: 0,
        };
        let mut counter = 0;

        assign_surface_render_generation(&mut counter, &mut request);
        assert_eq!(counter, 1);
        assert_eq!(request.generation, 1);

        counter = u64::MAX;
        assign_surface_render_generation(&mut counter, &mut request);
        assert_eq!(counter, 1);
        assert_eq!(request.generation, 1);
    }

    #[test]
    fn primary_surface_work_precedes_continuity_then_stays_fifo() {
        let mut priorities = [
            pending_surface_priority(8, SurfaceRenderRole::Continuity, true, 1),
            pending_surface_priority(4, SurfaceRenderRole::Continuity, false, 3),
            pending_surface_priority(3, SurfaceRenderRole::Primary, false, 2),
            pending_surface_priority(2, SurfaceRenderRole::Primary, true, 0),
        ];
        priorities.sort_unstable();

        assert_eq!(
            priorities,
            [
                pending_surface_priority(3, SurfaceRenderRole::Primary, false, 2),
                pending_surface_priority(4, SurfaceRenderRole::Continuity, false, 3),
                pending_surface_priority(2, SurfaceRenderRole::Primary, true, 0),
                pending_surface_priority(8, SurfaceRenderRole::Continuity, true, 1),
            ]
        );
    }

    #[test]
    fn surface_latency_samples_are_bounded_and_report_nearest_rank_p95() {
        let mut samples = VecDeque::new();
        for millis in 0..=MAX_SURFACE_LATENCY_SAMPLES {
            push_surface_latency(&mut samples, Duration::from_millis(millis as u64));
        }

        assert_eq!(samples.len(), MAX_SURFACE_LATENCY_SAMPLES);
        assert_eq!(samples.front(), Some(&Duration::from_millis(1)));
        assert_eq!(samples.back(), Some(&Duration::from_millis(120)));
        assert_eq!(surface_latency_p95(&samples), Duration::from_millis(114));
        assert_eq!(surface_latency_p95(&VecDeque::new()), Duration::ZERO);
    }

    #[test]
    fn backend_modes_keep_strict_pages_native_and_delay_compatibility_fallback() {
        assert!(mupdf_placeholder_allowed(
            PdfRenderEngine::MuPdf,
            false,
            false
        ));
        assert!(!mupdf_placeholder_allowed(
            PdfRenderEngine::Glyphst,
            true,
            true
        ));
        assert!(!mupdf_placeholder_allowed(
            PdfRenderEngine::GlyphstFirst,
            false,
            false
        ));
        assert!(mupdf_placeholder_allowed(
            PdfRenderEngine::GlyphstFirst,
            false,
            true
        ));
        assert!(mupdf_placeholder_allowed(
            PdfRenderEngine::GlyphstFirst,
            true,
            false
        ));
    }
}
