use super::*;

const MAX_RETAINED_GPU_PAGES: usize = 8;
const MAX_RETAINED_GPU_DIAGNOSTICS: usize = 64;
const MAX_CONCURRENT_GPU_PAGE_PREPARATIONS: usize = 2;
const MAX_RETAINED_GPU_RESOURCE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RETAINED_GPU_SURFACE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SURFACE_SEED_TASKS: usize = 2;

const GPU_MOTION_SETTLE_DELAY: Duration = Duration::from_millis(100);
const GPU_FIRST_PAINT_GRACE: Duration = Duration::from_millis(50);

fn same_patch_lod_request(existing: f32, required: f32) -> bool {
    existing.to_bits() == required.to_bits()
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
    presented_pages: HashSet<u32>,
    visible_pages: HashSet<u32>,
    application_fallbacks: HashMap<u32, Arc<[GpuFallbackDiagnostic]>>,
    link_mappings: HashMap<u32, CachedLinkMapping>,
    pub(super) reader_link_replacements: HashMap<u32, Vec<(usize, DocumentLink)>>,
    pub(super) reader_original_links: Option<Vec<DocumentLink>>,
    surface_failures: HashMap<u32, String>,
    page_last_used: HashMap<u32, u64>,
    page_usage_clock: u64,
    last_scroll_y_doc: Option<f32>,
    preparation_generation: u64,
    patch_lod_generation: u64,
    patch_lod_tasks: HashMap<u32, PatchLodTask>,
    surface_seed_generation: u64,
    surface_seed_tasks: HashMap<u32, SurfaceSeedTask>,
    fallback_grace_generation: u64,
    fallback_grace_tasks: HashMap<u32, FallbackGraceTask>,
    fallback_grace_elapsed: HashSet<u32>,
    render_generation: u64,
    motion_generation: u64,
    motion_active: bool,
    motion_deadline: Option<Instant>,
    motion_settle_task: Option<Task<()>>,
    destination_generation: u64,
    destination_cancellation: Option<GpuCancellationToken>,
    destination_task: Option<Task<()>>,
    destination_fallback: Option<Arc<[GpuFallbackDiagnostic]>>,
}

enum GpuPageState {
    Preparing {
        generation: u64,
        cancellation: GpuCancellationToken,
        _task: Task<()>,
    },
    Ready(Arc<PreparedGpuPage>),
    Fallback(Arc<[GpuFallbackDiagnostic]>),
    Failed(String),
}

enum CachedLinkMapping {
    Valid(Arc<[(usize, usize)]>),
    Invalid,
}

struct RenderedGpuPage {
    key: RenderKey,
    presentation: GpuPagePresentation,
    stats: GpuRenderStats,
}

struct PatchLodTask {
    generation: u64,
    required_scale: f32,
    cancellation: GpuCancellationToken,
    _task: Task<()>,
}

struct SurfaceSeedTask {
    generation: u64,
    key: RenderKey,
    region: GpuPageRegion,
    _task: Task<()>,
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
        self.presented_pages.clear();
        self.visible_pages.clear();
        self.application_fallbacks.clear();
        self.link_mappings.clear();
        self.reader_link_replacements.clear();
        self.reader_original_links = None;
        self.surface_failures.clear();
        self.page_last_used.clear();
        self.page_usage_clock = 0;
        self.last_scroll_y_doc = None;
        self.preparation_generation = 0;
        self.patch_lod_generation = 0;
        self.patch_lod_tasks.clear();
        self.surface_seed_generation = 0;
        self.surface_seed_tasks.clear();
        self.fallback_grace_generation = 0;
        self.fallback_grace_tasks.clear();
        self.fallback_grace_elapsed.clear();
        self.render_generation = 0;
        self.motion_generation = self.motion_generation.wrapping_add(1).max(1);
        self.motion_active = false;
        self.motion_deadline = None;
        self.motion_settle_task = None;
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
        self.presented_pages.clear();
        self.visible_pages.clear();
        self.application_fallbacks.clear();
        self.link_mappings.clear();
        self.surface_failures.clear();
        self.page_last_used.clear();
        self.page_usage_clock = 0;
        self.last_scroll_y_doc = None;
        self.preparation_generation = 0;
        self.patch_lod_generation = 0;
        self.patch_lod_tasks.clear();
        self.surface_seed_generation = 0;
        self.surface_seed_tasks.clear();
        self.fallback_grace_generation = 0;
        self.fallback_grace_tasks.clear();
        self.fallback_grace_elapsed.clear();
        self.render_generation = 0;
        self.motion_generation = self.motion_generation.wrapping_add(1).max(1);
        self.motion_active = false;
        self.motion_deadline = None;
        self.motion_settle_task = None;
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
        self.surface_seed_tasks.clear();
        self.fallback_grace_tasks.clear();
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
            || !self.surface_seed_tasks.is_empty()
            || !self.pages.is_empty()
            || !self.rendered.is_empty()
    }

    fn mark_page_used(&mut self, page: u32) {
        self.page_usage_clock = self.page_usage_clock.wrapping_add(1).max(1);
        self.page_last_used.insert(page, self.page_usage_clock);
    }

    fn prune(&mut self, visible: &HashSet<u32>, retained: &HashSet<u32>) {
        self.visible_pages.clone_from(visible);
        self.presented_pages.clear();
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
        self.surface_seed_tasks
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
            }
            self.page_last_used.remove(&page);
            self.rendered.remove(&page);
            self.presented_pages.remove(&page);
            self.application_fallbacks.remove(&page);
            self.link_mappings.remove(&page);
            self.surface_failures.remove(&page);
        }

        let incoming_visible = retained
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
        self.page_last_used
            .retain(|page, _| self.pages.contains_key(page));
        self.application_fallbacks
            .retain(|page, _| self.pages.contains_key(page));
        self.link_mappings
            .retain(|page, _| self.pages.contains_key(page));
        self.surface_failures
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
            self.presented_pages.remove(&page);
            self.application_fallbacks.remove(&page);
            self.link_mappings.remove(&page);
            self.surface_failures.remove(&page);
        }
    }

    fn surface_bytes(&self) -> u64 {
        self.rendered
            .values()
            .map(|rendered| rendered.presentation.byte_len())
            .sum()
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
            self.presented_pages.remove(&page);
            self.application_fallbacks.remove(&page);
            self.link_mappings.remove(&page);
            self.surface_failures.remove(&page);
        }
    }

    fn prune_surface_cache(&mut self) {
        while self.surface_bytes() > MAX_RETAINED_GPU_SURFACE_BYTES {
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
            let retained = self
                .rendered
                .iter()
                .filter(|(candidate, _)| **candidate != page)
                .map(|(_, rendered)| rendered.presentation.byte_len())
                .sum::<u64>();
            if retained.saturating_add(bytes) <= MAX_RETAINED_GPU_SURFACE_BYTES {
                return Ok(());
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

    fn has_preparation_capacity(&self) -> bool {
        let preparing = self
            .pages
            .values()
            .filter(|state| matches!(state, GpuPageState::Preparing { .. }))
            .count();
        preparing < MAX_CONCURRENT_GPU_PAGE_PREPARATIONS
            && self
                .pages
                .values()
                .filter(|state| {
                    matches!(
                        state,
                        GpuPageState::Preparing { .. } | GpuPageState::Ready(_)
                    )
                })
                .count()
                < MAX_RETAINED_GPU_PAGES
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

fn visible_gpu_region(
    key: RenderKey,
    image_bounds: Bounds<Pixels>,
    viewport_bounds: Bounds<Pixels>,
) -> Option<GpuPageRegion> {
    let image_x0 = image_bounds.origin.x.as_f32();
    let image_y0 = image_bounds.origin.y.as_f32();
    let image_width = image_bounds.size.width.as_f32();
    let image_height = image_bounds.size.height.as_f32();
    if image_width <= 0.0 || image_height <= 0.0 {
        return None;
    }
    let x0 = image_x0.max(viewport_bounds.origin.x.as_f32());
    let y0 = image_y0.max(viewport_bounds.origin.y.as_f32());
    let x1 = (image_x0 + image_width)
        .min(viewport_bounds.origin.x.as_f32() + viewport_bounds.size.width.as_f32());
    let y1 = (image_y0 + image_height)
        .min(viewport_bounds.origin.y.as_f32() + viewport_bounds.size.height.as_f32());
    if x1 <= x0 || y1 <= y0 {
        return None;
    }

    let page_width = key.physical_width as f32;
    let page_height = key.physical_height as f32;
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
    let page_width = key.physical_width.max(1) as f32;
    let page_height = key.physical_height.max(1) as f32;
    let logical_width = image_bounds.size.width.as_f32();
    let logical_height = image_bounds.size.height.as_f32();
    Bounds::new(
        point(
            image_bounds.origin.x + px(region.x as f32 / page_width * logical_width),
            image_bounds.origin.y + px(region.y as f32 / page_height * logical_height),
        ),
        size(
            px(region.width as f32 / page_width * logical_width),
            px(region.height as f32 / page_height * logical_height),
        ),
    )
}

fn gpu_paint_texture(
    key: RenderKey,
    image_bounds: Bounds<Pixels>,
    texture: GpuPageTexture,
) -> GpuPaintTexture {
    GpuPaintTexture {
        bounds: gpu_surface_bounds(key, image_bounds, texture.region()),
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

fn should_start_surface_seed(settled: bool, allow_motion_seed: bool, has_previous: bool) -> bool {
    settled || allow_motion_seed || !has_previous
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
                    this.gpu_pdf.motion_deadline = None;
                    this.gpu_pdf.motion_settle_task = None;
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

    pub(super) fn native_gpu_key_enabled(&self, key: RenderKey, engine: PdfRenderEngine) -> bool {
        engine.uses_native_reader()
            && self.format == DocumentFormat::Pdf
            && key.source_revision == 0
            && key.color_transform.is_none()
    }

    pub(super) fn native_gpu_texture(
        &mut self,
        key: RenderKey,
        image_bounds: Bounds<Pixels>,
        viewport_bounds: Bounds<Pixels>,
        refinement_budget: &mut bool,
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
            if engine.uses_native_reader() && self.format == DocumentFormat::Pdf {
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
        let visible_region = visible_gpu_region(key, image_bounds, viewport_bounds)?;
        if engine == PdfRenderEngine::GlyphstFirst {
            self.start_native_gpu_fallback_grace(page, cx);
        }

        let document = match self.gpu_pdf.document.clone() {
            Some(document) => document,
            None => {
                self.start_native_gpu_document_open(cx);
                return None;
            }
        };

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

        let allow_motion_seed = !self.gpu_pdf.visible_pages.contains(&page);
        self.gpu_pdf.mark_page_used(page);
        let prepared = match self.gpu_pdf.pages.get(&page) {
            Some(GpuPageState::Ready(page)) => Some(Arc::clone(page)),
            Some(GpuPageState::Preparing { .. })
            | Some(GpuPageState::Fallback(_))
            | Some(GpuPageState::Failed(_)) => None,
            None => {
                if !self.gpu_pdf.has_preparation_capacity() {
                    return None;
                }
                self.start_native_gpu_page_prepare(
                    page,
                    Arc::clone(&document),
                    Arc::clone(&renderer),
                    cx,
                );
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

        let settled = !self.dock_snapshot.layout_motion_active
            && self.panning.is_none()
            && !self.gpu_pdf.motion_active
            && (self.scroll_x_doc - self.target_scroll_x_doc).abs() <= 0.01
            && (self.scroll_y_doc - self.target_scroll_y_doc).abs() <= 0.01;
        self.gpu_pdf.render_generation = self.gpu_pdf.render_generation.wrapping_add(1).max(1);
        let request = GpuPaintRequest {
            page_size: [key.physical_width, key.physical_height],
            visible_region,
            rotation: key.rotation,
            background: [1.0, 1.0, 1.0, 1.0],
            settled,
            generation: self.gpu_pdf.render_generation,
        };
        let mut presentation = self
            .gpu_pdf
            .rendered
            .get(&page)
            .map(|rendered| rendered.presentation.clone())
            .unwrap_or_default();
        let mut surface_region = match presentation.surface_region(request) {
            Ok(region) => region,
            Err(error) => {
                self.gpu_pdf
                    .surface_failures
                    .insert(page, format!("{error:#}"));
                return None;
            }
        };
        let patch_lod_status = match renderer.patch_lod_status(&prepared, request) {
            Ok(status) => status,
            Err(error) => {
                self.gpu_pdf
                    .surface_failures
                    .insert(page, format!("{error:#}"));
                return None;
            }
        };
        if patch_lod_status.ready {
            if let Some(task) = self.gpu_pdf.patch_lod_tasks.remove(&page) {
                task.cancellation.cancel();
            }
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
                if let Some(rendered) = self.gpu_pdf.rendered.get(&page) {
                    let rendered_key = rendered.key;
                    let texture = rendered.presentation.current_texture()?;
                    self.gpu_pdf.presented_pages.insert(page);
                    return Some(gpu_paint_texture(rendered_key, image_bounds, texture));
                }
                return None;
            }
        }

        if self.gpu_pdf.surface_seed_tasks.contains_key(&page) {
            if let Some(rendered) = self.gpu_pdf.rendered.get(&page)
                && let Some(texture) = rendered.presentation.current_texture()
            {
                let rendered_key = rendered.key;
                self.gpu_pdf.presented_pages.insert(page);
                return Some(gpu_paint_texture(rendered_key, image_bounds, texture));
            }
            return None;
        }

        if let Some(rendered) = self.gpu_pdf.rendered.get(&page)
            && rendered.key == key
            && rendered.presentation.token() == Some(renderer.token())
            && rendered
                .presentation
                .current_texture()
                .is_some_and(|texture| texture.region() == surface_region)
            && (!settled || !rendered.presentation.refinement_pending())
        {
            let texture = rendered.presentation.current_texture()?;
            self.gpu_pdf.presented_pages.insert(page);
            self.gpu_pdf.surface_failures.remove(&page);
            return Some(gpu_paint_texture(key, image_bounds, texture));
        }

        let work = match renderer.plan_page(&prepared, request, &mut presentation) {
            Ok(work) => work,
            Err(error) => {
                self.gpu_pdf
                    .surface_failures
                    .insert(page, format!("{error:#}"));
                return None;
            }
        };
        surface_region = match presentation.surface_region(request) {
            Ok(region) => region,
            Err(error) => {
                self.gpu_pdf
                    .surface_failures
                    .insert(page, format!("{error:#}"));
                return None;
            }
        };
        if work.kind == GpuIncrementalWorkKind::Refine && !*refinement_budget {
            let rendered = self.gpu_pdf.rendered.get(&page)?;
            let texture = rendered.presentation.current_texture()?;
            self.gpu_pdf.presented_pages.insert(page);
            window.request_animation_frame();
            return Some(gpu_paint_texture(rendered.key, image_bounds, texture));
        }
        let requested_surface_bytes = presentation
            .projected_byte_len(surface_region, work.kind == GpuIncrementalWorkKind::Direct);
        if let Err(required) = self.gpu_pdf.reserve_surface(page, requested_surface_bytes) {
            self.gpu_pdf.surface_failures.insert(
                page,
                format!(
                    "visible native PDF surfaces require {} bytes, exceeding the {}-byte cache limit",
                    required, MAX_RETAINED_GPU_SURFACE_BYTES
                ),
            );
            return None;
        }
        if work.kind == GpuIncrementalWorkKind::Full {
            let previous = self.gpu_pdf.rendered.get(&page).and_then(|rendered| {
                rendered
                    .presentation
                    .current_texture()
                    .map(|texture| (rendered.key, texture))
            });
            // Motion may defer replacing an already usable native texture, but
            // it must not starve a newly visible page behind a MuPDF
            // placeholder. Adjacent warming normally makes this branch cheap;
            // a cold jump still starts one bounded, visible-priority seed.
            if should_start_surface_seed(settled, allow_motion_seed, previous.is_some()) {
                self.start_native_gpu_surface_seed(
                    page,
                    key,
                    Arc::clone(&prepared),
                    Arc::clone(&renderer),
                    request,
                    surface_region,
                    presentation,
                    cx,
                );
            }
            if let Some((rendered_key, texture)) = previous {
                self.gpu_pdf.presented_pages.insert(page);
                return Some(gpu_paint_texture(rendered_key, image_bounds, texture));
            }
            return None;
        }
        if work.kind == GpuIncrementalWorkKind::Refine {
            *refinement_budget = false;
        }
        self.gpu_pdf.surface_failures.remove(&page);
        match renderer.render_page(&prepared, request, &mut presentation) {
            Ok((texture, stats)) => {
                let refinement_pending = presentation.refinement_pending();
                self.gpu_pdf.rendered.insert(
                    page,
                    RenderedGpuPage {
                        key,
                        presentation,
                        stats,
                    },
                );
                self.gpu_pdf.prune_surface_cache();
                self.gpu_pdf.presented_pages.insert(page);
                if refinement_pending {
                    // Exact analytic and scope-free retained work is bounded
                    // per settled frame. Keep the reprojected texture visible
                    // and ask GPUI for the next paint until the engine reports
                    // complete exact coverage.
                    window.request_animation_frame();
                }
                Some(gpu_paint_texture(key, image_bounds, texture))
            }
            Err(error) => {
                self.gpu_pdf
                    .surface_failures
                    .insert(page, format!("{error:#}"));
                eprintln!(
                    "failed to render native PDF page {} on the GPU: {error:#}",
                    page + 1
                );
                if renderer.is_device_lost() {
                    self.gpu_pdf.reset_device();
                }
                None
            }
        }
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
                    }
                    Err(error) => {
                        this.gpu_pdf.fallback_grace_tasks.remove(&page);
                        this.gpu_pdf.fallback_grace_elapsed.remove(&page);
                        eprintln!(
                            "failed to prepare native PDF page {} for the GPU: {error}",
                            page + 1
                        );
                        this.gpu_pdf.pages.insert(page, GpuPageState::Failed(error));
                    }
                }
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
                _task: task,
            },
        );
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn start_native_gpu_surface_seed(
        &mut self,
        page: u32,
        key: RenderKey,
        prepared: Arc<PreparedGpuPage>,
        renderer: Arc<GpuPdfRenderer>,
        request: GpuPaintRequest,
        region: GpuPageRegion,
        mut presentation: GpuPagePresentation,
        cx: &mut Context<Self>,
    ) {
        if self.gpu_pdf.surface_seed_tasks.contains_key(&page) {
            return;
        }
        if self.gpu_pdf.surface_seed_tasks.len() >= MAX_SURFACE_SEED_TASKS {
            if self.gpu_pdf.visible_pages.contains(&page) {
                let offscreen = self
                    .gpu_pdf
                    .surface_seed_tasks
                    .keys()
                    .find(|candidate| !self.gpu_pdf.visible_pages.contains(candidate))
                    .copied();
                if let Some(offscreen) = offscreen {
                    self.gpu_pdf.surface_seed_tasks.remove(&offscreen);
                } else {
                    return;
                }
            } else {
                return;
            }
        }
        let document_generation = self.gpu_pdf.generation;
        self.gpu_pdf.surface_seed_generation =
            self.gpu_pdf.surface_seed_generation.wrapping_add(1).max(1);
        let generation = self.gpu_pdf.surface_seed_generation;
        let token = renderer.token();
        let worker_page = Arc::clone(&prepared);
        let worker_renderer = Arc::clone(&renderer);
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    worker_renderer
                        .render_page(&worker_page, request, &mut presentation)
                        .map(|(_, stats)| (presentation, stats))
                        .map_err(|error| format!("{error:#}"))
                })
                .await;
            let update_result = this.update(cx, |this, cx| {
                if this.gpu_pdf.generation != document_generation
                    || !matches!(
                        this.gpu_pdf.surface_seed_tasks.get(&page),
                        Some(task)
                            if task.generation == generation
                                && task.key == key
                                && task.region == region
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
                this.gpu_pdf.surface_seed_tasks.remove(&page);
                match result {
                    Ok((presentation, stats)) => {
                        let bytes = presentation.byte_len();
                        if let Err(required) = this.gpu_pdf.reserve_surface(page, bytes) {
                            this.gpu_pdf.surface_failures.insert(
                                page,
                                format!(
                                    "visible native PDF surfaces require {} bytes, exceeding the {}-byte cache limit",
                                    required, MAX_RETAINED_GPU_SURFACE_BYTES
                                ),
                            );
                        } else {
                            this.gpu_pdf.rendered.insert(
                                page,
                                RenderedGpuPage {
                                    key,
                                    presentation,
                                    stats,
                                },
                            );
                            this.gpu_pdf.surface_failures.remove(&page);
                            this.gpu_pdf.prune_surface_cache();
                        }
                    }
                    Err(error) => {
                        eprintln!(
                            "failed to seed native PDF page {} GPU surface: {error}",
                            page + 1
                        );
                        this.gpu_pdf.surface_failures.insert(page, error);
                        if this
                            .gpu_pdf
                            .renderer
                            .as_ref()
                            .is_some_and(|renderer| renderer.is_device_lost())
                        {
                            this.gpu_pdf.reset_device();
                        }
                    }
                }
                cx.notify();
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
                _task: task,
            },
        );
        cx.notify();
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
        let first = visible.iter().min().copied();
        let last = visible.iter().max().copied();
        let forward = last.and_then(|page| {
            page.checked_add(1)
                .filter(|page| (*page as usize) < self.pages.len())
        });
        let backward = first.and_then(|page| page.checked_sub(1));
        let adjacent = if direction < 0.0 {
            backward.or(forward)
        } else {
            forward.or(backward)
        };
        let mut retained = visible.clone();
        retained.extend(adjacent);
        self.gpu_pdf.prune(&visible, &retained);
        let adjacent_index = adjacent.and_then(|page| usize::try_from(page).ok());

        let engine = cx
            .global::<SettingsStore>()
            .settings
            .document_view
            .pdf_render_engine;
        if !engine.uses_native_reader() || self.format != DocumentFormat::Pdf {
            return None;
        }
        let (Some(page), Some(document), Some(renderer)) = (
            adjacent,
            self.gpu_pdf.document.clone(),
            self.gpu_pdf.renderer.clone(),
        ) else {
            return adjacent_index;
        };
        if !self.gpu_pdf.pages.contains_key(&page) && self.gpu_pdf.has_preparation_capacity() {
            self.gpu_pdf.mark_page_used(page);
            self.start_native_gpu_page_prepare(page, document, renderer, cx);
        }
        adjacent_index
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
        let Ok(page) = u32::try_from(key.page_idx) else {
            return;
        };
        if self.gpu_pdf.rendered.contains_key(&page)
            || self.gpu_pdf.surface_seed_tasks.contains_key(&page)
        {
            return;
        }
        let predicted_viewport =
            adjacent_gpu_viewport_bounds(image_bounds, viewport_bounds, forward);
        let mut refinement_budget = true;
        _ = self.native_gpu_texture(
            key,
            image_bounds,
            predicted_viewport,
            &mut refinement_budget,
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
        self.gpu_pdf
            .rendered
            .get(&page)
            .map(|rendered| (rendered.key, rendered.stats))
    }

    pub(super) fn native_gpu_preparation_stats(&self, page: usize) -> Option<GpuPreparationStats> {
        let page = u32::try_from(page).ok()?;
        match self.gpu_pdf.pages.get(&page)? {
            GpuPageState::Ready(page) => Some(page.preparation_stats()),
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
                .sum(),
            surface_bytes: self.gpu_pdf.surface_bytes(),
            surface_byte_limit: MAX_RETAINED_GPU_SURFACE_BYTES,
            surface_seed_tasks: self.gpu_pdf.surface_seed_tasks.len(),
            refinement_pages: self
                .gpu_pdf
                .rendered
                .values()
                .filter(|rendered| rendered.presentation.refinement_pending())
                .count(),
            motion_active: self.gpu_pdf.motion_active,
        }
    }

    pub(super) fn native_gpu_failure(&self, page: usize) -> Option<&str> {
        let page = u32::try_from(page).ok()?;
        if let Some(error) = self.gpu_pdf.surface_failures.get(&page) {
            return Some(error);
        }
        match self.gpu_pdf.pages.get(&page) {
            Some(GpuPageState::Failed(error)) => Some(error),
            _ => self
                .gpu_pdf
                .renderer_error
                .as_deref()
                .or(self.gpu_pdf.document_error.as_deref()),
        }
    }

    pub(super) fn native_gpu_pending(&self, page: usize) -> bool {
        let Some(page) = u32::try_from(page).ok() else {
            return false;
        };
        self.gpu_pdf.document_task.is_some()
            || matches!(
                self.gpu_pdf.pages.get(&page),
                Some(GpuPageState::Preparing { .. })
            )
            || self.gpu_pdf.patch_lod_tasks.contains_key(&page)
            || self.gpu_pdf.surface_seed_tasks.contains_key(&page)
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
    fn patch_lod_worker_is_reused_only_for_the_same_declared_scale_slot() {
        assert!(same_patch_lod_request(4.0, 4.0));
        assert!(!same_patch_lod_request(4.0, 8.0));
        assert!(!same_patch_lod_request(8.0, 4.0));
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
    fn cold_visible_pages_are_not_starved_during_motion() {
        assert!(should_start_surface_seed(false, false, false));
        assert!(!should_start_surface_seed(false, false, true));
        assert!(should_start_surface_seed(false, true, true));
        assert!(should_start_surface_seed(true, false, true));
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
