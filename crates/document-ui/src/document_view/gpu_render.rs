use super::*;

const MAX_RETAINED_GPU_PAGES: usize = 8;
const MAX_RETAINED_GPU_DIAGNOSTICS: usize = 64;
const MAX_RETAINED_GPU_SURFACE_BYTES: u64 = 256 * 1024 * 1024;
const GPU_MOTION_SETTLE_DELAY: Duration = Duration::from_millis(100);

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
    surface_failures: HashMap<u32, String>,
    page_last_used: HashMap<u32, u64>,
    page_usage_clock: u64,
    preparation_generation: u64,
    render_generation: u64,
    motion_generation: u64,
    motion_active: bool,
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

struct RenderedGpuPage {
    key: RenderKey,
    texture: GpuPageTexture,
    stats: GpuRenderStats,
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
        self.surface_failures.clear();
        self.page_last_used.clear();
        self.page_usage_clock = 0;
        self.preparation_generation = 0;
        self.render_generation = 0;
        self.motion_generation = self.motion_generation.wrapping_add(1).max(1);
        self.motion_active = false;
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
        self.surface_failures.clear();
        self.page_last_used.clear();
        self.page_usage_clock = 0;
        self.preparation_generation = 0;
        self.render_generation = 0;
        self.motion_generation = self.motion_generation.wrapping_add(1).max(1);
        self.motion_active = false;
        self.motion_settle_task = None;
        self.destination_fallback = None;
    }

    fn cancel_page_tasks(&mut self) {
        for state in self.pages.values() {
            if let GpuPageState::Preparing { cancellation, .. } = state {
                cancellation.cancel();
            }
        }
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
            || !self.pages.is_empty()
            || !self.rendered.is_empty()
    }

    fn mark_page_used(&mut self, page: u32) {
        self.page_usage_clock = self.page_usage_clock.wrapping_add(1).max(1);
        self.page_last_used.insert(page, self.page_usage_clock);
    }

    fn prune(&mut self, visible: &HashSet<u32>) {
        self.visible_pages.clone_from(visible);
        self.presented_pages.clear();
        let stale_preparations = self
            .pages
            .iter()
            .filter_map(|(page, state)| {
                (!visible.contains(page) && matches!(state, GpuPageState::Preparing { .. }))
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
            self.surface_failures.remove(&page);
        }

        let incoming_visible = visible
            .iter()
            .filter(|page| !self.pages.contains_key(page))
            .count();
        let retained_limit = MAX_RETAINED_GPU_PAGES.saturating_sub(incoming_visible);
        self.prune_states(visible, retained_limit, |state| {
            matches!(state, GpuPageState::Ready(_))
        });
        self.prune_states(visible, MAX_RETAINED_GPU_DIAGNOSTICS, |state| {
            matches!(state, GpuPageState::Fallback(_) | GpuPageState::Failed(_))
        });
        self.rendered
            .retain(|page, _| self.pages.contains_key(page));
        self.page_last_used
            .retain(|page, _| self.pages.contains_key(page));
        self.application_fallbacks
            .retain(|page, _| self.pages.contains_key(page));
        self.surface_failures
            .retain(|page, _| self.pages.contains_key(page));
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
            self.surface_failures.remove(&page);
        }
    }

    fn surface_bytes(&self) -> u64 {
        self.rendered
            .values()
            .map(|rendered| rendered.texture.byte_len())
            .sum()
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
                .map(|(_, rendered)| rendered.texture.byte_len())
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
        self.pages
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

impl DocumentView {
    pub(super) fn mark_native_gpu_motion(&mut self, cx: &mut Context<Self>) {
        self.gpu_pdf.motion_generation = self.gpu_pdf.motion_generation.wrapping_add(1).max(1);
        self.gpu_pdf.motion_active = true;
        let generation = self.gpu_pdf.motion_generation;
        self.gpu_pdf.motion_settle_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(GPU_MOTION_SETTLE_DELAY)
                .await;
            _ = this.update(cx, |this, cx| {
                if this.gpu_pdf.motion_generation != generation {
                    return;
                }
                this.gpu_pdf.motion_active = false;
                this.gpu_pdf.motion_settle_task = None;
                cx.notify();
            });
        }));
    }

    pub(super) fn native_gpu_key_enabled(&self, key: RenderKey, engine: PdfRenderEngine) -> bool {
        engine != PdfRenderEngine::MuPdf
            && self.format == DocumentFormat::Pdf
            && key.source_revision == 0
            && key.color_transform.is_none()
    }

    pub(super) fn native_gpu_texture(
        &mut self,
        key: RenderKey,
        image_bounds: Bounds<Pixels>,
        viewport_bounds: Bounds<Pixels>,
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
                self.gpu_pdf.reset();
            }
            return None;
        }

        // The current native slice is fixed-layout PDF only. Translated page
        // revisions and document color transforms remain whole-page MuPDF
        // routes until their semantics are implemented by the native graph.
        if !self.native_gpu_key_enabled(key, engine) {
            return None;
        }
        let visible_region = visible_gpu_region(key, image_bounds, viewport_bounds)?;

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

        let page = u32::try_from(key.page_idx).ok()?;
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

        if Self::native_link_mapping(key.page_idx, &self.links, prepared.link_interactions())
            .is_none()
        {
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
        let surface_region = match request.surface_region() {
            Ok(region) => region,
            Err(error) => {
                self.gpu_pdf
                    .surface_failures
                    .insert(page, format!("{error:#}"));
                return None;
            }
        };

        if let Some(rendered) = self.gpu_pdf.rendered.get(&page)
            && rendered.key == key
            && rendered.texture.token() == renderer.token()
            && rendered.texture.region() == surface_region
            && (!settled || !rendered.texture.refinement_pending())
        {
            let texture = rendered.texture.clone();
            self.gpu_pdf.presented_pages.insert(page);
            self.gpu_pdf.surface_failures.remove(&page);
            return Some(gpu_paint_texture(key, image_bounds, texture));
        }

        if let Err(required) = self
            .gpu_pdf
            .reserve_surface(page, surface_region.byte_len())
        {
            self.gpu_pdf.surface_failures.insert(
                page,
                format!(
                    "visible native PDF surfaces require {} bytes, exceeding the {}-byte cache limit",
                    required, MAX_RETAINED_GPU_SURFACE_BYTES
                ),
            );
            return None;
        }
        self.gpu_pdf.surface_failures.remove(&page);
        let previous = self.gpu_pdf.rendered.remove(&page);
        match renderer.render_page(
            &prepared,
            request,
            previous.as_ref().map(|rendered| &rendered.texture),
        ) {
            Ok((texture, stats)) => {
                self.gpu_pdf.rendered.insert(
                    page,
                    RenderedGpuPage {
                        key,
                        texture: texture.clone(),
                        stats,
                    },
                );
                self.gpu_pdf.prune_surface_cache();
                self.gpu_pdf.presented_pages.insert(page);
                Some(gpu_paint_texture(key, image_bounds, texture))
            }
            Err(error) => {
                if let Some(previous) = previous {
                    self.gpu_pdf.rendered.insert(page, previous);
                }
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
                        this.gpu_pdf
                            .pages
                            .insert(page, GpuPageState::Ready(prepared));
                    }
                    Ok(GpuPageRoute::Fallback(diagnostics)) => {
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

    pub(super) fn prune_native_gpu_pages(&mut self, visible_pages: &[usize]) {
        let visible = visible_pages
            .iter()
            .filter_map(|page| u32::try_from(*page).ok())
            .collect::<HashSet<_>>();
        self.gpu_pdf.prune(&visible);
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

    pub(super) fn native_gpu_cache_debug(&self) -> GpuCacheDebugSnapshot {
        let prepared = self
            .gpu_pdf
            .pages
            .values()
            .filter_map(|state| match state {
                GpuPageState::Ready(page) => Some(page.preparation_stats()),
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
            resident_bytes: prepared.iter().map(|stats| stats.resident_bytes).sum(),
            surface_count: self.gpu_pdf.rendered.len(),
            surface_bytes: self.gpu_pdf.surface_bytes(),
            surface_byte_limit: MAX_RETAINED_GPU_SURFACE_BYTES,
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
}
