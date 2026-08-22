use super::*;

const MAX_RETAINED_GPU_PAGES: usize = 8;
const MAX_RETAINED_GPU_DIAGNOSTICS: usize = 64;

#[derive(Default)]
pub(super) struct GpuPdfState {
    generation: u64,
    document: Option<Arc<GpuPdfDocument>>,
    document_task: Option<Task<()>>,
    document_error: Option<String>,
    device_token: Option<ExternalGpuDeviceToken>,
    renderer: Option<Arc<GpuPdfRenderer>>,
    renderer_error: Option<String>,
    pages: HashMap<u32, GpuPageState>,
    rendered: HashMap<u32, RenderedGpuPage>,
    page_last_used: HashMap<u32, u64>,
    page_usage_clock: u64,
    preparation_generation: u64,
    render_generation: u64,
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
        self.generation = self.generation.wrapping_add(1);
        self.document = None;
        self.document_task = None;
        self.document_error = None;
        self.device_token = None;
        self.renderer = None;
        self.renderer_error = None;
        self.pages.clear();
        self.rendered.clear();
        self.page_last_used.clear();
        self.page_usage_clock = 0;
        self.preparation_generation = 0;
        self.render_generation = 0;
    }

    fn reset_device(&mut self) {
        self.cancel_page_tasks();
        self.generation = self.generation.wrapping_add(1);
        self.renderer = None;
        self.renderer_error = None;
        self.pages.clear();
        self.rendered.clear();
        self.page_last_used.clear();
        self.page_usage_clock = 0;
        self.preparation_generation = 0;
        self.render_generation = 0;
    }

    fn cancel_page_tasks(&mut self) {
        for state in self.pages.values() {
            if let GpuPageState::Preparing { cancellation, .. } = state {
                cancellation.cancel();
            }
        }
    }

    fn is_active(&self) -> bool {
        self.document.is_some()
            || self.document_task.is_some()
            || self.document_error.is_some()
            || self.device_token.is_some()
            || self.renderer.is_some()
            || self.renderer_error.is_some()
            || !self.pages.is_empty()
            || !self.rendered.is_empty()
    }

    fn mark_page_used(&mut self, page: u32) {
        self.page_usage_clock = self.page_usage_clock.wrapping_add(1).max(1);
        self.page_last_used.insert(page, self.page_usage_clock);
    }

    fn prune(&mut self, visible: &HashSet<u32>) {
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

impl DocumentView {
    pub(super) fn native_gpu_key_enabled(&self, key: RenderKey, engine: PdfRenderEngine) -> bool {
        engine != PdfRenderEngine::MuPdf
            && self.format == DocumentFormat::Pdf
            && key.source_revision == 0
            && key.color_transform.is_none()
    }

    pub(super) fn native_gpu_texture(
        &mut self,
        key: RenderKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<GpuPageTexture> {
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

        if let Some(rendered) = self.gpu_pdf.rendered.get(&page)
            && rendered.key == key
            && rendered.texture.token() == renderer.token()
        {
            return Some(rendered.texture.clone());
        }

        let previous = self.gpu_pdf.rendered.remove(&page);
        let reusable = previous.as_ref().map(|rendered| rendered.texture.clone());
        self.gpu_pdf.render_generation = self.gpu_pdf.render_generation.wrapping_add(1).max(1);
        let request = GpuPaintRequest {
            target_size: [key.physical_width, key.physical_height],
            rotation: key.rotation,
            background: [1.0, 1.0, 1.0, 1.0],
            settled: !self.dock_snapshot.layout_motion_active && self.panning.is_none(),
            generation: self.gpu_pdf.render_generation,
        };
        match renderer.render_page(&prepared, request, reusable) {
            Ok((texture, stats)) => {
                self.gpu_pdf.rendered.insert(
                    page,
                    RenderedGpuPage {
                        key,
                        texture: texture.clone(),
                        stats,
                    },
                );
                Some(texture)
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
        match self.gpu_pdf.pages.get(&page)? {
            GpuPageState::Fallback(diagnostics) => Some(diagnostics),
            _ => None,
        }
    }

    pub(super) fn native_gpu_render(&self, page: usize) -> Option<(RenderKey, GpuRenderStats)> {
        let page = u32::try_from(page).ok()?;
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
            prepared_pages: prepared.len(),
            prepared_page_limit: MAX_RETAINED_GPU_PAGES,
            resident_bytes: prepared.iter().map(|stats| stats.resident_bytes).sum(),
        }
    }

    pub(super) fn native_gpu_failure(&self, page: usize) -> Option<&str> {
        let page = u32::try_from(page).ok()?;
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
