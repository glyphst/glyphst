//! Camera geometry, coherent surface selection, and native page painting.

use super::*;

pub(super) fn use_current_camera_render(visible: bool, exact_cache_covers: bool) -> bool {
    visible && !exact_cache_covers
}

pub(super) fn visible_gpu_region(
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

pub(super) fn gpu_surface_bounds(
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

pub(super) fn gpu_paint_texture(
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

pub(super) fn texture_page_paint(texture: GpuPaintTexture) -> GpuPagePaint {
    GpuPagePaint {
        texture: Some(texture),
        direct: None,
    }
}

pub(super) fn direct_page_paint(direct: GpuDirectPaint) -> GpuPagePaint {
    GpuPagePaint {
        texture: None,
        direct: Some(direct),
    }
}

pub(super) fn gpu_debug_bounds(bounds: Bounds<Pixels>) -> [f32; 4] {
    [
        bounds.origin.x.as_f32(),
        bounds.origin.y.as_f32(),
        (bounds.origin.x + bounds.size.width).as_f32(),
        (bounds.origin.y + bounds.size.height).as_f32(),
    ]
}

pub(super) fn adjacent_gpu_viewport_bounds(
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
    pub(super) const fn is_speculative(self) -> bool {
        matches!(self, Self::AdjacentWarm)
    }
}

pub(super) fn should_start_gpu_fallback_grace(
    engine: PdfRenderEngine,
    intent: GpuSurfaceIntent,
) -> bool {
    engine == PdfRenderEngine::GlyphstFirst && intent == GpuSurfaceIntent::Visible
}

pub(super) fn should_demand_missing_gpu_page(intent: GpuSurfaceIntent) -> bool {
    intent == GpuSurfaceIntent::Visible
}

#[derive(Clone, Copy)]
pub(super) struct GpuPaintTargetPlan {
    pub(super) pan_capacity: Option<[u32; 2]>,
    pub(super) pan_axes: [bool; 2],
    pub(super) full_page_pan_axes: [bool; 2],
    pub(super) camera_capacity: Option<[u32; 2]>,
}

pub(super) fn gpu_paint_request(
    key: RenderKey,
    visible_region: GpuPageRegion,
    settled: bool,
    targets: GpuPaintTargetPlan,
    camera_serial: u64,
    device_origin_phase: [f32; 2],
) -> GpuPaintRequest {
    GpuPaintRequest {
        page_size: [key.physical_width, key.physical_height],
        visible_region,
        target_capacity: targets.pan_capacity,
        pan_cache_axes: targets.pan_axes,
        full_page_pan_cache_axes: targets.full_page_pan_axes,
        camera_target_capacity: targets.camera_capacity,
        camera_serial,
        device_origin_phase,
        rotation: key.rotation,
        background: [1.0, 1.0, 1.0, 1.0],
        settled,
        // A render generation is assigned only after exact-cache and
        // scheduler coalescing checks prove this is material new work.
        generation: 0,
    }
}

pub(super) fn round_half_toward_zero(value: f32) -> f32 {
    (value.abs() - 0.5).ceil().copysign(value)
}

pub(super) fn gpu_device_origin_phase(image_bounds: Bounds<Pixels>, scale_factor: f32) -> [f32; 2] {
    let scale_factor = scale_factor.max(0.001);
    [image_bounds.origin.x, image_bounds.origin.y].map(|origin| {
        let physical = origin.as_f32() * scale_factor;
        physical - round_half_toward_zero(physical)
    })
}

pub(super) fn physical_gpu_viewport_size(
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

pub(super) fn directional_pan_cache_axes(
    previous: Option<(GpuPageRegion, [bool; 2])>,
    visible: GpuPageRegion,
) -> [bool; 2] {
    let Some((previous, previous_axes)) = previous else {
        // Document readers overwhelmingly begin with vertical translation.
        // The cross axis still keeps the fixed 96 px safety border.
        return [false, true];
    };
    let delta = [
        previous.x.abs_diff(visible.x),
        previous.y.abs_diff(visible.y),
    ];
    if previous_axes == [false, true] && delta[0] <= GPU_PAN_AXIS_JITTER_PX {
        return [false, true];
    }
    if previous_axes == [true, false] && delta[1] <= GPU_PAN_AXIS_JITTER_PX {
        return [true, false];
    }
    match (delta[0] > 0, delta[1] > 0) {
        (true, true) if delta[0].saturating_mul(4) <= delta[1] => [false, true],
        (true, true) if delta[1].saturating_mul(4) <= delta[0] => [true, false],
        (true, true) => [true, true],
        (true, false) => [true, false],
        (false, true) => [false, true],
        (false, false) => previous_axes,
    }
}

pub(super) fn stable_gpu_target_capacity(
    key: RenderKey,
    image_bounds: Bounds<Pixels>,
    viewport_bounds: Bounds<Pixels>,
    dimension_limit: u32,
    pan_cache_axes: [bool; 2],
) -> Option<[u32; 2]> {
    if dimension_limit == 0 {
        return None;
    }
    let viewport = physical_gpu_viewport_size(key, image_bounds, viewport_bounds)?;
    let mut capacity = [0_u32; 2];
    for axis in 0..2 {
        let gutter = if pan_cache_axes[axis] {
            viewport[axis]
                .saturating_mul(7)
                .div_ceil(20)
                .max(GPU_PAN_CACHE_GUTTER_MIN)
        } else {
            GPU_PAN_CACHE_GUTTER_MIN
        };
        capacity[axis] = viewport[axis]
            .saturating_add(gutter.saturating_mul(2))
            .div_ceil(GPU_TARGET_ALIGNMENT)
            .saturating_mul(GPU_TARGET_ALIGNMENT)
            .min(dimension_limit);
    }
    Some(capacity)
}

pub(super) fn stable_gpu_camera_target_capacity(
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

pub(super) fn rgba_surface_bytes(size: [u32; 2]) -> Option<u64> {
    u64::from(size[0])
        .checked_mul(u64::from(size[1]))?
        .checked_mul(4)
}

pub(super) fn bounded_full_page_pan_cache_axes(
    page_size: [u32; 2],
    base_pan_capacity: [u32; 2],
    camera_capacity: [u32; 2],
    pan_cache_axes: [bool; 2],
    preparation: GpuPreparationStats,
    dimension_limit: u32,
) -> [bool; 2] {
    if preparation.motion_stroke_lod_segments < GPU_FULL_PAGE_CACHE_MIN_MOTION_SEGMENTS {
        return [false; 2];
    }

    let mut full_axes = [false; 2];
    let mut pan_capacity = base_pan_capacity;
    for axis in 0..2 {
        if !pan_cache_axes[axis] || page_size[axis] > dimension_limit {
            continue;
        }
        let mut candidate = pan_capacity;
        candidate[axis] = page_size[axis];
        let Some(required) = rgba_surface_bytes(candidate)
            .and_then(|bytes| bytes.checked_mul(GPU_PAN_CACHE_TARGET_COUNT))
            .and_then(|pan_bytes| {
                rgba_surface_bytes(camera_capacity)
                    .and_then(|bytes| bytes.checked_mul(GPU_CAMERA_TARGET_COUNT))
                    .and_then(|camera_bytes| pan_bytes.checked_add(camera_bytes))
            })
        else {
            continue;
        };
        if required <= MAX_RETAINED_GPU_SURFACE_BYTES {
            full_axes[axis] = true;
            pan_capacity = candidate;
        }
    }
    full_axes
}

pub(super) fn full_page_pan_cache_covers_axes(
    request: GpuPaintRequest,
    active_axes: [bool; 2],
) -> bool {
    (0..2).all(|axis| !active_axes[axis] || request.full_page_pan_cache_axes[axis])
}

pub(super) fn should_warm_adjacent_surface(
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

pub(super) fn speculative_surface_fits(
    protected_bytes: u64,
    target_bytes: u64,
    limit: u64,
) -> bool {
    protected_bytes
        .checked_add(target_bytes)
        .is_some_and(|total| total <= limit)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SurfaceCachePressureAction {
    PreserveDiagnostics,
    ClearDiagnostics,
    RouteToFallback,
}

pub(super) fn surface_cache_pressure_action(
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

pub(super) fn prewarmed_surface_covers(
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

pub(super) fn should_start_surface_seed(
    settled: bool,
    allow_motion_seed: bool,
    has_previous: bool,
) -> bool {
    settled || allow_motion_seed || !has_previous
}

pub(super) fn should_defer_full_surface_render(
    settled: bool,
    allow_motion_seed: bool,
    has_previous: bool,
) -> bool {
    !should_start_surface_seed(settled, allow_motion_seed, has_previous)
}

pub(super) fn assign_surface_render_generation(counter: &mut u64, request: &mut GpuPaintRequest) {
    *counter = counter.wrapping_add(1).max(1);
    request.generation = *counter;
}

pub(super) fn same_gpu_surface_content(left: GpuPaintRequest, right: GpuPaintRequest) -> bool {
    // The visible region, motion quality, and request generation choose work;
    // they do not change the identity of already-rendered page pixels.
    left.page_size == right.page_size
        && left.target_capacity == right.target_capacity
        && left.pan_cache_axes == right.pan_cache_axes
        && left.full_page_pan_cache_axes == right.full_page_pan_cache_axes
        && left.camera_target_capacity == right.camera_target_capacity
        && left.device_origin_phase == right.device_origin_phase
        && left.rotation == right.rotation
        && left.background == right.background
}

pub(super) fn same_gpu_surface_work(left: GpuPaintRequest, right: GpuPaintRequest) -> bool {
    same_gpu_surface_content(left, right) && left.settled == right.settled
}

pub(super) fn same_gpu_page_source(left: RenderKey, right: RenderKey) -> bool {
    left.doc_id == right.doc_id
        && left.page_idx == right.page_idx
        && left.source_revision == right.source_revision
        && left.rotation == right.rotation
        && left.color_transform == right.color_transform
}

pub(super) fn same_gpu_page_dimensions(left: RenderKey, right: RenderKey) -> bool {
    left.physical_width == right.physical_width && left.physical_height == right.physical_height
}

pub(super) fn translation_cache_compatible(cached: RenderKey, requested: RenderKey) -> bool {
    same_gpu_page_source(cached, requested) && same_gpu_page_dimensions(cached, requested)
}

pub(super) fn motion_surface_density_sufficient(rendered: RenderKey, requested: RenderKey) -> bool {
    same_gpu_page_source(rendered, requested)
        && u64::from(rendered.physical_width).saturating_mul(GPU_MOTION_MIN_DENSITY_DENOMINATOR)
            >= u64::from(requested.physical_width).saturating_mul(GPU_MOTION_MIN_DENSITY_NUMERATOR)
        && u64::from(rendered.physical_height).saturating_mul(GPU_MOTION_MIN_DENSITY_DENOMINATOR)
            >= u64::from(requested.physical_height).saturating_mul(GPU_MOTION_MIN_DENSITY_NUMERATOR)
}

pub(super) fn gpu_region_contains(outer: GpuPageRegion, inner: GpuPageRegion) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

pub(super) fn gpu_regions_overlap(left: GpuPageRegion, right: GpuPageRegion) -> bool {
    left.x < right.right()
        && right.x < left.right()
        && left.y < right.bottom()
        && right.y < left.bottom()
}

pub(super) fn presented_surface_kind(
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

pub(super) fn zoom_motion_cancels_surface_work(
    has_coherent_surface: bool,
    role: SurfaceRenderRole,
    speculative: bool,
) -> bool {
    speculative || role == SurfaceRenderRole::Continuity || has_coherent_surface
}

/// Keeps the original visible region in the middle half of every available
/// overscan margin. Crossing this guard starts the next surface while the
/// current texture still covers the viewport.
pub(super) fn surface_refresh_guard(
    surface: GpuPageRegion,
    seed_visible: GpuPageRegion,
) -> GpuPageRegion {
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

pub(super) fn surface_request_covers(
    existing: SurfaceRequestIdentity,
    incoming: SurfaceRequestIdentity,
) -> bool {
    existing.key == incoming.key
        && same_gpu_surface_work(existing.request, incoming.request)
        && gpu_region_contains(existing.region, incoming.request.visible_region)
}

pub(super) fn rendered_surface_covers_request(
    existing: SurfaceRequestIdentity,
    incoming: SurfaceRequestIdentity,
) -> bool {
    existing.key == incoming.key
        && same_gpu_surface_work(existing.request, incoming.request)
        && gpu_region_contains(existing.region, incoming.region)
}

pub(super) fn can_coalesce_pending_surface_request(
    pending: SurfaceRequestIdentity,
    pending_speculative: bool,
    incoming: SurfaceRequestIdentity,
    incoming_speculative: bool,
) -> bool {
    surface_request_covers(pending, incoming) && (!pending_speculative || incoming_speculative)
}

pub(super) fn can_reuse_active_surface_request(
    same_request: bool,
    active_speculative: bool,
    incoming_speculative: bool,
) -> bool {
    same_request && (!active_speculative || incoming_speculative)
}

pub(super) fn mupdf_placeholder_allowed(
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
    pub(in super::super) fn strict_native_pdf(&self, cx: &gpui::App) -> bool {
        self.format == DocumentFormat::Pdf
            && cx
                .global::<SettingsStore>()
                .settings
                .document_view
                .pdf_render_engine
                .is_strict_native()
    }

    pub(in super::super) fn native_gpu_document(&self) -> Option<Arc<GpuPdfDocument>> {
        self.gpu_pdf.document.clone()
    }

    pub(super) fn start_native_gpu_fallback_grace(&mut self, page: u32, cx: &mut Context<Self>) {
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

    pub(in super::super) fn native_gpu_mupdf_placeholder_allowed(
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

    pub(in super::super) fn native_gpu_key_enabled(
        &self,
        key: RenderKey,
        engine: PdfRenderEngine,
    ) -> bool {
        engine.uses_native_reader()
            && self.format == DocumentFormat::Pdf
            && key.source_revision == 0
            && key.color_transform.is_none()
    }

    pub(super) fn selected_gpu_surface(
        &self,
        page: u32,
        requested_key: RenderKey,
        image_bounds: Bounds<Pixels>,
        viewport_bounds: Bounds<Pixels>,
        renderer_token: ExternalGpuDeviceToken,
        current: Option<&CurrentGpuSurface>,
    ) -> (Option<SelectedGpuSurface>, bool) {
        let primary_covers = current.is_some_and(|current| {
            translation_cache_compatible(current.key, requested_key)
                && gpu_region_contains(current.texture.region(), current.visible_region)
        });
        let primary_overlaps = current.is_some_and(|current| {
            translation_cache_compatible(current.key, requested_key)
                && gpu_regions_overlap(current.texture.region(), current.visible_region)
        });
        let sharp_was_presented = self
            .gpu_pdf
            .rendered
            .get(&page)
            .is_some_and(|rendered| rendered.presented_once);
        let continuity = (!sharp_was_presented)
            .then(|| {
                self.gpu_pdf
                    .continuity_surfaces
                    .get(&page)
                    .and_then(|surface| {
                        if !translation_cache_compatible(surface.key, requested_key)
                            || surface.texture.token() != renderer_token
                        {
                            return None;
                        }
                        let visible_region =
                            visible_gpu_region(surface.key, image_bounds, viewport_bounds)?;
                        gpu_region_contains(surface.texture.region(), visible_region)
                            .then_some(surface)
                    })
            })
            .flatten();
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

    pub(super) fn present_gpu_surface(
        &mut self,
        page: u32,
        image_bounds: Bounds<Pixels>,
        selected: SelectedGpuSurface,
    ) -> GpuPagePaint {
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
        if let Some(enqueued_at) = enqueued_at {
            self.gpu_pdf.record_surface_present_latency(
                Instant::now().saturating_duration_since(enqueued_at),
            );
        }
        let paint = gpu_paint_texture(selected.key, image_bounds, selected.texture);
        self.gpu_pdf.presentation_debug.insert(
            page,
            GpuPresentationDebugSnapshot {
                source: match selected.kind {
                    PresentedSurfaceKind::Primary => "exact-surface",
                    PresentedSurfaceKind::Continuity => "continuity-surface",
                    PresentedSurfaceKind::PartialPrimary => "partial-surface",
                }
                .into(),
                page_bounds: gpu_debug_bounds(image_bounds),
                texture_bounds: Some(gpu_debug_bounds(paint.bounds)),
                direct_bounds: None,
                texture_physical_size: Some([
                    selected.key.physical_width,
                    selected.key.physical_height,
                ]),
                camera_serial: self.gpu_pdf.motion_camera_serial,
                vector_lod_requested_scale: None,
                vector_lod_available_scale: None,
                vector_lod_ready: None,
            },
        );
        if matches!(
            selected.kind,
            PresentedSurfaceKind::Primary | PresentedSurfaceKind::PartialPrimary
        ) && let Some(rendered) = self.gpu_pdf.rendered.get_mut(&page)
        {
            rendered.presented_once = true;
        }
        texture_page_paint(paint)
    }

    pub(super) fn present_selected_or_record_gap(
        &mut self,
        page: u32,
        image_bounds: Bounds<Pixels>,
        selected: Option<SelectedGpuSurface>,
    ) -> Option<GpuPagePaint> {
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

    pub(super) fn reconcile_direct_render_receipt(&mut self, page: u32) {
        let Some(cached) = self.gpu_pdf.direct_renders.get(&page) else {
            return;
        };
        if cached.reconciled {
            return;
        }
        let outcome = cached.receipt.outcome();
        if outcome == GpuDirectRenderOutcome::Pending {
            return;
        }
        let receipt = cached.receipt.clone();
        self.gpu_pdf
            .direct_renders
            .get_mut(&page)
            .expect("the observed direct render still exists")
            .reconciled = true;
        match outcome {
            GpuDirectRenderOutcome::Encoded => {
                self.gpu_pdf.presented_pages.insert(page);
                self.gpu_pdf.covered_pages.insert(page);
                self.gpu_pdf.surface_failures.remove(&page);
                self.gpu_pdf.motion_render_inline_frames =
                    self.gpu_pdf.motion_render_inline_frames.saturating_add(1);
                self.gpu_pdf.surface_zoom_live_redraws =
                    self.gpu_pdf.surface_zoom_live_redraws.saturating_add(1);
                if self.gpu_pdf.motion_presented_camera_serial != receipt.camera_serial() {
                    self.gpu_pdf.motion_presented_camera_serial = receipt.camera_serial();
                    self.gpu_pdf.motion_camera_batch_commits =
                        self.gpu_pdf.motion_camera_batch_commits.saturating_add(1);
                }
            }
            GpuDirectRenderOutcome::Failed => {
                self.gpu_pdf.motion_render_inline_failures =
                    self.gpu_pdf.motion_render_inline_failures.saturating_add(1);
                self.gpu_pdf.surface_zoom_live_failures =
                    self.gpu_pdf.surface_zoom_live_failures.saturating_add(1);
                self.gpu_pdf.surface_failures.insert(
                    page,
                    format!(
                        "current-camera native PDF draw failed for generation {}",
                        receipt.generation()
                    ),
                );
            }
            GpuDirectRenderOutcome::Superseded | GpuDirectRenderOutcome::Pending => {}
        }
        if outcome != GpuDirectRenderOutcome::Encoded {
            self.gpu_pdf.direct_renders.remove(&page);
        }
    }

    pub(in super::super) fn native_gpu_paint(
        &mut self,
        key: RenderKey,
        image_bounds: Bounds<Pixels>,
        viewport_bounds: Bounds<Pixels>,
        intent: GpuSurfaceIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<GpuPagePaint> {
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
        self.reconcile_direct_render_receipt(page);
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
        let device_origin_phase = gpu_device_origin_phase(image_bounds, window.scale_factor());
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
        let pan_cache_axes = directional_pan_cache_axes(
            self.gpu_pdf.rendered.get(&page).and_then(|rendered| {
                (same_gpu_page_source(rendered.key, key)
                    && same_gpu_page_dimensions(rendered.key, key))
                .then_some((
                    rendered.request.visible_region,
                    rendered.request.pan_cache_axes,
                ))
            }),
            exact_visible_region,
        );
        let computed_target_capacity = stable_gpu_target_capacity(
            key,
            image_bounds,
            viewport_bounds,
            renderer.max_texture_dimension_2d(),
            pan_cache_axes,
        );
        let computed_camera_target_capacity = stable_gpu_camera_target_capacity(
            key,
            image_bounds,
            viewport_bounds,
            renderer.max_texture_dimension_2d(),
        );
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
        let full_page_pan_axes = match (computed_target_capacity, computed_camera_target_capacity) {
            (Some(pan_capacity), Some(camera_capacity)) => bounded_full_page_pan_cache_axes(
                [key.physical_width, key.physical_height],
                pan_capacity,
                camera_capacity,
                pan_cache_axes,
                prepared.preparation_stats(),
                renderer.max_texture_dimension_2d(),
            ),
            _ => [false; 2],
        };
        let target_capacity = computed_target_capacity.map(|mut capacity| {
            let page_size = [key.physical_width, key.physical_height];
            for axis in 0..2 {
                if full_page_pan_axes[axis] {
                    capacity[axis] = page_size[axis];
                }
            }
            capacity
        });
        let paint_targets = GpuPaintTargetPlan {
            pan_capacity: target_capacity,
            pan_axes: pan_cache_axes,
            full_page_pan_axes,
            camera_capacity: None,
        };

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

        let settled = !self.dock_snapshot.layout_motion_active
            && self.panning.is_none()
            && !self.gpu_pdf.motion_active
            && (self.scroll_x_doc - self.target_scroll_x_doc).abs() <= f64::EPSILON
            && (self.scroll_y_doc - self.target_scroll_y_doc).abs() <= f64::EPSILON;
        // Compare the live layout with the retained page in the retained
        // page's own physical coordinate space. Scale-changing motion keeps
        // the latest completed sharp camera frame visible while its detached
        // successor renders.
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
                paint_targets,
                self.gpu_pdf.motion_camera_serial,
                device_origin_phase,
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
        let (selected_surface, continuity_covers) = self.selected_gpu_surface(
            page,
            key,
            image_bounds,
            viewport_bounds,
            renderer.token(),
            current_surface.as_ref(),
        );

        let scale_changed = !current_dimensions_exact;
        let exact_cache_covers = (current_dimensions_exact && current_covers) || continuity_covers;
        let dimension_motion = !settled && scale_changed;
        let fixed_scale_pan = !dimension_motion && current_dimensions_exact;
        let mut camera_request = gpu_paint_request(
            key,
            exact_visible_region,
            false,
            paint_targets,
            self.gpu_pdf.motion_camera_serial,
            device_origin_phase,
        );
        camera_request.camera_target_capacity = computed_camera_target_capacity;
        let camera_vector_lod_status = scale_changed
            .then(|| renderer.vector_lod_status(&prepared, camera_request))
            .transpose()
            .ok()
            .flatten();
        if !speculative
            && let Some(status) = camera_vector_lod_status
            && !status.ready
            && let Some(required_scale) = next_vector_lod_scale(
                status.available_scale,
                motion_vector_lod_scale(
                    status.requested_scale,
                    self.zoom,
                    self.target_zoom,
                    MAX_ZOOM,
                ),
            )
        {
            self.start_native_gpu_vector_lod_prepare(
                page,
                Arc::clone(&prepared),
                Arc::clone(&renderer),
                required_scale,
                cx,
            );
        }
        let camera_vector_lod_ready =
            !scale_changed || camera_vector_lod_status.is_some_and(|status| status.ready);
        let camera_patch_lod_status = (settled && camera_vector_lod_ready)
            .then(|| renderer.patch_lod_status(&prepared, camera_request))
            .transpose()
            .ok()
            .flatten();
        if let Some(status) = camera_patch_lod_status
            && !status.ready
        {
            self.start_native_gpu_patch_lod_prepare(
                page,
                Arc::clone(&prepared),
                Arc::clone(&renderer),
                camera_request,
                status.required_scale,
                cx,
            );
        }
        if use_current_camera_render(intent == GpuSurfaceIntent::Visible, exact_cache_covers) {
            // Cached pixels are a translation-only optimization. When they do
            // not fully cover this camera, draw retained vectors into GPUI's
            // current frame instead of stretching or partially exposing them.
            let bounds = gpu_surface_bounds(key, image_bounds, exact_visible_region);
            let identity = DirectRenderIdentity {
                key,
                visible_region: exact_visible_region,
                device_origin_phase,
                camera_serial: camera_request.camera_serial,
                bounds,
            };
            let presentation = GpuPresentationDebugSnapshot {
                source: "direct-current".into(),
                page_bounds: gpu_debug_bounds(image_bounds),
                texture_bounds: None,
                direct_bounds: Some(gpu_debug_bounds(bounds)),
                texture_physical_size: None,
                camera_serial: camera_request.camera_serial,
                vector_lod_requested_scale: camera_vector_lod_status
                    .map(|status| status.requested_scale),
                vector_lod_available_scale: camera_vector_lod_status
                    .map(|status| status.available_scale),
                vector_lod_ready: camera_vector_lod_status.map(|status| status.ready),
            };
            if let Some(paint) = self
                .gpu_pdf
                .direct_renders
                .get(&page)
                .filter(|cached| {
                    cached.identity == identity && Arc::ptr_eq(&cached.prepared, &prepared)
                })
                .map(|cached| cached.paint.clone())
            {
                self.gpu_pdf.presentation_debug.insert(page, presentation);
                return Some(direct_page_paint(paint));
            }
            let mut request = camera_request;
            assign_surface_render_generation(&mut self.gpu_pdf.render_generation, &mut request);
            let direct = renderer
                .supersede_page_render(&prepared, request.generation)
                .and_then(|()| renderer.direct_render_handle(Arc::clone(&prepared), request));
            match direct {
                Ok(submission) => {
                    let paint = GpuDirectPaint {
                        bounds,
                        render: submission.render,
                    };
                    self.gpu_pdf.direct_renders.insert(
                        page,
                        CachedDirectRender {
                            identity,
                            prepared: Arc::clone(&prepared),
                            paint: paint.clone(),
                            receipt: submission.receipt,
                            reconciled: false,
                        },
                    );
                    self.gpu_pdf.presentation_debug.insert(page, presentation);
                    return Some(direct_page_paint(paint));
                }
                Err(error) => {
                    self.gpu_pdf.motion_render_inline_failures =
                        self.gpu_pdf.motion_render_inline_failures.saturating_add(1);
                    self.gpu_pdf.surface_zoom_live_failures =
                        self.gpu_pdf.surface_zoom_live_failures.saturating_add(1);
                    self.gpu_pdf
                        .surface_failures
                        .insert(page, format!("{error:#}"));
                    if renderer.is_device_lost() {
                        self.gpu_pdf.reset_device();
                        return None;
                    }
                    eprintln!(
                        "failed to prepare same-frame native PDF zoom redraw on page {}: {error:#}",
                        page + 1
                    );
                }
            }
        }
        if self.gpu_pdf.zoom_motion_active
            && let Some(rendered) = self.gpu_pdf.rendered.get_mut(&page)
        {
            // Physical-size equality means the completed pixels are already
            // the exact output for this animation step. Promote only the
            // serial instead of queuing a duplicate subpixel redraw.
            rendered.request.camera_serial = self.gpu_pdf.motion_camera_serial;
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
                    || (fixed_scale_pan
                        && full_page_pan_cache_covers_axes(
                            current.request,
                            paint_targets.pan_axes,
                        ))
                    || (!fixed_scale_pan
                        && current
                            .target_region
                            .is_some_and(|target| current.texture.region() == target)))
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
                if fixed_scale_pan
                    && !self.gpu_pdf.zoom_motion_active
                    && self.gpu_pdf.motion_presented_camera_serial
                        != self.gpu_pdf.motion_camera_serial
                {
                    self.gpu_pdf.surface_sharp_pan_cache_presentations = self
                        .gpu_pdf
                        .surface_sharp_pan_cache_presentations
                        .saturating_add(1);
                    self.gpu_pdf.motion_presented_camera_serial = self.gpu_pdf.motion_camera_serial;
                }
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
        let surface_phase = if fixed_scale_pan {
            current_surface
                .as_ref()
                .map_or(device_origin_phase, |current| {
                    current.request.device_origin_phase
                })
        } else {
            device_origin_phase
        };
        let request = gpu_paint_request(
            render_key,
            visible_region,
            true,
            paint_targets,
            self.gpu_pdf.motion_camera_serial,
            surface_phase,
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
        if speculative {
            return None;
        }
        if let Some(selected) = selected_surface {
            if intent == GpuSurfaceIntent::Visible
                && fixed_scale_pan
                && !self.gpu_pdf.zoom_motion_active
                && selected.kind == PresentedSurfaceKind::Primary
                && self.gpu_pdf.motion_presented_camera_serial != self.gpu_pdf.motion_camera_serial
            {
                self.gpu_pdf.surface_sharp_pan_cache_presentations = self
                    .gpu_pdf
                    .surface_sharp_pan_cache_presentations
                    .saturating_add(1);
                self.gpu_pdf.motion_presented_camera_serial = self.gpu_pdf.motion_camera_serial;
            }
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

    pub(in super::super) fn native_gpu_link_interactions(
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

    pub(in super::super) fn cached_native_gpu_link_mapping(
        &self,
        page: usize,
    ) -> Option<&[(usize, usize)]> {
        let page = u32::try_from(page).ok()?;
        match self.gpu_pdf.link_mappings.get(&page)? {
            CachedLinkMapping::Valid(mapping) => Some(mapping),
            CachedLinkMapping::Invalid => None,
        }
    }
}
