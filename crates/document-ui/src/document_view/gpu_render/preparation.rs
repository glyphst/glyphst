//! Demand ordering and generation-scoped background preparation.

use super::*;

pub(super) fn same_patch_lod_request(existing: f32, required: f32) -> bool {
    existing.to_bits() == required.to_bits()
}

pub(super) fn next_vector_lod_scale(available: f32, requested: f64) -> Option<f32> {
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
    (requested.is_finite() && requested > available).then_some(requested)
}

pub(super) fn motion_vector_lod_scale(
    requested: f64,
    zoom: f32,
    target_zoom: f32,
    max_zoom: f32,
) -> f64 {
    if !requested.is_finite()
        || zoom <= 0.0
        || !zoom.is_finite()
        || !target_zoom.is_finite()
        || !max_zoom.is_finite()
    {
        return requested;
    }
    if target_zoom <= zoom {
        return requested;
    }
    let projected = requested * f64::from(target_zoom / zoom);
    let headroom = requested * f64::from((max_zoom / zoom).clamp(1.0, 2.0));
    requested.max(projected.max(headroom) * 1.01)
}

pub(super) fn vector_lod_request_covers(existing: f32, required: f32) -> bool {
    // Requests reserve up to 2x camera headroom. Reuse that worker until the
    // live demand actually reaches its prepared scale instead of cancelling
    // it for every small increase in the speculative headroom target.
    existing.is_finite()
        && required.is_finite()
        && f64::from(existing) * 2.0 >= f64::from(required) * 0.999_999
}

pub(super) fn presented_refinement_page_count(
    presented_pages: &HashSet<u32>,
    pending_pages: impl IntoIterator<Item = u32>,
) -> usize {
    pending_pages
        .into_iter()
        .filter(|page| presented_pages.contains(page))
        .count()
}

pub(super) fn gpu_preparation_targets(
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

pub(super) fn speculative_preparation_allowed(
    motion_active: bool,
    background_paused: bool,
    active_speculative: usize,
) -> bool {
    !background_paused && (!motion_active || active_speculative == 0)
}

pub(super) fn pending_surface_priority(
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

impl DocumentView {
    pub(in super::super) fn mark_native_gpu_motion(&mut self, cx: &mut Context<Self>) {
        if let Some(document) = self.gpu_pdf.document.as_ref() {
            document.note_interaction();
        }
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
                    this.finish_native_gpu_motion(cx);
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

    pub(in super::super) fn settle_native_gpu_motion_if_due(&mut self, cx: &mut Context<Self>) {
        if self.gpu_pdf.motion_active
            && self
                .gpu_pdf
                .motion_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.finish_native_gpu_motion(cx);
        }
    }

    pub(super) fn finish_native_gpu_motion(&mut self, cx: &mut Context<Self>) {
        self.gpu_pdf.motion_active = false;
        self.gpu_pdf.zoom_motion_active = false;
        self.gpu_pdf.motion_deadline = None;
        self.gpu_pdf.motion_settle_task = None;
        self.gpu_pdf.motion_retained_pages.clear();
        self.schedule_native_gpu_speculative_resume(cx);
        self.dispatch_native_gpu_page_preparations(cx);
        self.dispatch_native_gpu_surface_render(cx);
        cx.notify();
    }

    pub(super) fn schedule_native_gpu_speculative_resume(&mut self, cx: &mut Context<Self>) {
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

    pub(in super::super) fn mark_native_gpu_zoom_motion(&mut self, cx: &mut Context<Self>) {
        self.gpu_pdf.begin_zoom_motion();
        self.mark_native_gpu_motion(cx);
    }

    pub(in super::super) fn record_native_gpu_camera_step(&mut self, scale_changed: bool) {
        if self.gpu_pdf.renderer.is_some() {
            if scale_changed {
                self.gpu_pdf.surface_zoom_animation_steps =
                    self.gpu_pdf.surface_zoom_animation_steps.saturating_add(1);
            }
            self.gpu_pdf.motion_camera_serial =
                self.gpu_pdf.motion_camera_serial.wrapping_add(1).max(1);
        }
    }

    pub(super) fn start_native_gpu_document_open(&mut self, cx: &mut Context<Self>) {
        if self.gpu_pdf.document_task.is_some() || self.gpu_pdf.document_error.is_some() {
            return;
        }
        let snapshot = Arc::clone(&self.document_snapshot);
        let expected_page_count = self.page_count;
        let generation = self.gpu_pdf.generation;
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    GpuPdfDocument::open_with_compiled_cache(
                        &snapshot,
                        glyphst_core::paths::state_dir()
                            .join("pdf-gpu")
                            .join(format!("compiled-v{}", glyphst_pdf_gpu::COMPILED_PAGE_CACHE_VERSION)),
                    )
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

    pub(super) fn start_native_gpu_page_prepare(
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

    pub(super) fn native_gpu_preparation_motion_active(&self) -> bool {
        self.dock_snapshot.layout_motion_active
            || self.panning.is_some()
            || self.gpu_pdf.motion_active
            || (self.scroll_x_doc - self.target_scroll_x_doc).abs() > f64::EPSILON
            || (self.scroll_y_doc - self.target_scroll_y_doc).abs() > f64::EPSILON
    }

    pub(super) fn dispatch_native_gpu_page_preparations(&mut self, cx: &mut Context<Self>) {
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn enqueue_native_gpu_surface_render(
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

        if request.generation == 0 {
            assign_surface_render_generation(&mut self.gpu_pdf.render_generation, &mut request);
        }
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

    pub(super) fn dispatch_native_gpu_surface_render(&mut self, cx: &mut Context<Self>) {
        if self.gpu_pdf.zoom_motion_active
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
            if pending.request.settled {
                let status = match pending
                    .renderer
                    .vector_lod_status(&pending.prepared, pending.request)
                {
                    Ok(status) => status,
                    Err(error) => {
                        self.gpu_pdf
                            .surface_failures
                            .insert(page, format!("{error:#}"));
                        continue;
                    }
                };
                if !status.ready {
                    self.gpu_pdf.surface_render_deferred =
                        self.gpu_pdf.surface_render_deferred.saturating_add(1);
                    if let Some(required_scale) =
                        next_vector_lod_scale(status.available_scale, status.requested_scale)
                    {
                        self.start_native_gpu_vector_lod_prepare(
                            page,
                            Arc::clone(&pending.prepared),
                            Arc::clone(&pending.renderer),
                            required_scale,
                            cx,
                        );
                    }
                    continue;
                }
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
                                rendered_surface_covers_request(
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

    pub(super) fn start_native_gpu_surface_seed(
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
                        let completed_presentation = presented.is_some();
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
                                            // Existing pixels remain usable, but they do not
                                            // prove that the failed replacement succeeded.
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
                                                presented_once: false,
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
                                                    presented_once: false,
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
                                    if completed_presentation {
                                        this.gpu_pdf.surface_failures.remove(&page);
                                    }
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

    pub(super) fn start_native_gpu_vector_lod_prepare(
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
            .is_some_and(|task| vector_lod_request_covers(task.required_scale, required_scale))
            || self
                .gpu_pdf
                .vector_lod_failures
                .get(&page)
                .is_some_and(|failure| {
                    vector_lod_request_covers(failure.required_scale, required_scale)
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

    pub(super) fn start_native_gpu_patch_lod_prepare(
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
                    Ok(_) => {}
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
    pub(in super::super) fn start_native_gpu_action_resolution(
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

    pub(in super::super) fn prune_native_gpu_pages(
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
        let direction = if target_delta.abs() > f64::EPSILON {
            target_delta
        } else {
            observed_delta
        };
        if direction.abs() > f64::EPSILON {
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
            let target_bottom = target_top + f64::from(self.viewport_height / scale);
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

    pub(in super::super) fn warm_native_gpu_surface(
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
        let Some(renderer) = self.gpu_pdf.renderer.as_ref() else {
            return;
        };
        let candidate_ready = matches!(self.gpu_pdf.pages.get(&page), Some(GpuPageState::Ready(_)));
        if self.gpu_pdf.surface_seed_tasks.contains_key(&page)
            || self.gpu_pdf.surface_render_pending.contains_key(&page)
        {
            return;
        }
        let predicted_viewport =
            adjacent_gpu_viewport_bounds(image_bounds, viewport_bounds, forward);
        let Some(visible_region) = visible_gpu_region(key, image_bounds, predicted_viewport) else {
            return;
        };
        if self.gpu_pdf.rendered.get(&page).is_some_and(|rendered| {
            same_gpu_page_dimensions(rendered.key, key)
                && rendered.presentation.token() == Some(renderer.token())
                && rendered
                    .presentation
                    .current_texture()
                    .is_some_and(|texture| gpu_region_contains(texture.region(), visible_region))
        }) {
            return;
        }
        let target_capacity = stable_gpu_target_capacity(
            key,
            image_bounds,
            predicted_viewport,
            renderer.max_texture_dimension_2d(),
            [false, true],
        );
        let request = gpu_paint_request(
            key,
            visible_region,
            true,
            GpuPaintTargetPlan {
                pan_capacity: target_capacity,
                pan_axes: [false, true],
                full_page_pan_axes: [false; 2],
                camera_capacity: None,
            },
            self.gpu_pdf.motion_camera_serial,
            gpu_device_origin_phase(image_bounds, window.scale_factor()),
        );
        let Ok(surface_region) = request.surface_region() else {
            return;
        };
        let allocated_size = target_capacity.map_or(surface_region.size(), |capacity| {
            [
                capacity[0].max(surface_region.width),
                capacity[1].max(surface_region.height),
            ]
        });
        let target_bytes = u64::from(allocated_size[0])
            .saturating_mul(u64::from(allocated_size[1]))
            .saturating_mul(4);
        let retained_surface_bytes = self.gpu_pdf.surface_bytes();
        let budget_fits = speculative_surface_fits(
            retained_surface_bytes,
            target_bytes,
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

        _ = self.native_gpu_paint(
            key,
            image_bounds,
            predicted_viewport,
            GpuSurfaceIntent::AdjacentWarm,
            window,
            cx,
        );
    }
}
