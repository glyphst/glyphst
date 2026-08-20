use super::*;
use futures::future::try_join_all;
use std::sync::{Arc as StdArc, atomic::AtomicBool};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RasterBandRange {
    pub(super) y0: u32,
    pub(super) y1: u32,
}

struct ShadeDetector {
    found: StdArc<AtomicBool>,
}

impl mupdf::NativeDevice for ShadeDetector {
    fn fill_shade(
        &mut self,
        _shade: &mupdf::Shade,
        _matrix: Matrix,
        _alpha: f32,
        _color_params: ColorParams,
    ) {
        self.found.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

struct RasterBand {
    range: RasterBandRange,
    samples: Vec<u8>,
    draw_started_at: Instant,
    draw_finished_at: Instant,
}

pub(super) struct RasterizedSamples {
    pub(super) samples: Vec<u8>,
    pub(super) mupdf_draw: Duration,
    pub(super) raster_bands: usize,
}

struct RenderedPage {
    image: Arc<RenderImage>,
    mupdf_draw: Duration,
    raster_bands: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PixelRect {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

fn pixel_rect_for_transformed_bounds(
    bounds: Rect,
    matrix: &Matrix,
    width: usize,
    height: usize,
) -> Option<PixelRect> {
    let bounds = bounds.transform(matrix);
    if ![bounds.x0, bounds.y0, bounds.x1, bounds.y1]
        .into_iter()
        .all(f32::is_finite)
    {
        return None;
    }

    let x0 = bounds.x0.floor().clamp(0.0, width as f32) as usize;
    let y0 = bounds.y0.floor().clamp(0.0, height as f32) as usize;
    let x1 = bounds.x1.ceil().clamp(0.0, width as f32) as usize;
    let y1 = bounds.y1.ceil().clamp(0.0, height as f32) as usize;
    (x0 < x1 && y0 < y1).then_some(PixelRect { x0, y0, x1, y1 })
}

fn apply_pdf_duotone(
    samples: &mut [u8],
    width: usize,
    height: usize,
    transform: PdfColorTransform,
    image_exclusions: &[PixelRect],
) {
    let mut image_exclusions = image_exclusions.to_vec();
    image_exclusions.sort_unstable_by_key(|rect| rect.x0);

    let channel = |color: u32, shift: u32| ((color >> shift) & 0xffu32) as f32;
    let foreground = [
        channel(transform.foreground, 16),
        channel(transform.foreground, 8),
        channel(transform.foreground, 0),
    ];
    let background = [
        channel(transform.background, 16),
        channel(transform.background, 8),
        channel(transform.background, 0),
    ];

    let mut transform_range = |y: usize, x0: usize, x1: usize| {
        let row_start = y * width * 4;
        for x in x0..x1 {
            let offset = row_start + x * 4;
            let red = samples[offset] as f32;
            let green = samples[offset + 1] as f32;
            let blue = samples[offset + 2] as f32;
            let luminance = (0.2126 * red + 0.7152 * green + 0.0722 * blue) / 255.0;
            for component in 0..3 {
                samples[offset + component] = (foreground[component] * (1.0 - luminance)
                    + background[component] * luminance)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    };

    for y in 0..height {
        let mut cursor = 0;
        for rect in &image_exclusions {
            if y < rect.y0 || y >= rect.y1 || rect.x1 <= cursor {
                continue;
            }
            if rect.x0 > cursor {
                transform_range(y, cursor, rect.x0.min(width));
            }
            cursor = cursor.max(rect.x1).min(width);
            if cursor == width {
                break;
            }
        }
        if cursor < width {
            transform_range(y, cursor, width);
        }
    }
}

pub(super) fn should_start_viewport_render(
    has_pending_render: bool,
    has_failed_render: bool,
    request_exact: bool,
    has_fallback: bool,
) -> bool {
    !has_pending_render && !has_failed_render && (request_exact || !has_fallback)
}

pub(super) fn should_show_page_loading(
    has_pending_render: bool,
    has_displayed_render: bool,
) -> bool {
    has_pending_render && !has_displayed_render
}

pub(super) fn raster_band_count(
    has_shading: bool,
    pixel_count: u64,
    height: u32,
    available_cpus: usize,
) -> usize {
    if !has_shading || pixel_count < MIN_PARALLEL_SHADE_PIXELS || available_cpus < 2 || height == 0
    {
        return 1;
    }

    let height_limited = height.div_ceil(MIN_RASTER_BAND_HEIGHT) as usize;
    available_cpus
        .min(MAX_RASTER_BANDS)
        .min(height_limited)
        .max(1)
}

pub(super) fn raster_band_ranges(height: u32, band_count: usize) -> Vec<RasterBandRange> {
    if height == 0 || band_count == 0 {
        return Vec::new();
    }

    (0..band_count)
        .map(|band_idx| {
            let y0 = (u64::from(height) * band_idx as u64 / band_count as u64) as u32;
            let y1 = (u64::from(height) * (band_idx + 1) as u64 / band_count as u64) as u32;
            RasterBandRange { y0, y1 }
        })
        .filter(|range| range.y0 < range.y1)
        .collect()
}

impl DocumentView {
    pub(super) fn update_render_cache_protection(&self, cx: &mut Context<Self>) {
        let mut protected_renders = HashSet::new();
        if !self.grid_overview_open {
            protected_renders.extend(self.viewport_render_keys.iter().copied());
        }
        if self.dock_snapshot.left_sidebar == Some(DocumentSidebar::Thumbnails) {
            protected_renders.extend(self.thumbnail_sidebar_render_keys.values());
        }
        if self.grid_overview_open {
            protected_renders.extend(self.grid_overview_render_keys.values());
        }
        cx.global_mut::<RenderCache>()
            .set_protected_renders(self.doc_id, protected_renders);
    }

    fn overview_render_keys_mut(&mut self, surface: PageOverviewSurface) -> &mut SurfaceRenderKeys {
        match surface {
            PageOverviewSurface::ThumbnailSidebar => &mut self.thumbnail_sidebar_render_keys,
            PageOverviewSurface::GridOverview => &mut self.grid_overview_render_keys,
        }
    }

    pub(super) fn register_overview_render_key(
        &mut self,
        surface: PageOverviewSurface,
        page_idx: usize,
        key: RenderKey,
        cx: &mut Context<Self>,
    ) {
        if self
            .overview_render_keys_mut(surface)
            .register(page_idx, key)
        {
            self.update_render_cache_protection(cx);
        }
    }

    pub(super) fn retain_overview_render_pages(
        &mut self,
        surface: PageOverviewSurface,
        visible_pages: impl IntoIterator<Item = usize>,
        cx: &mut Context<Self>,
    ) {
        if self
            .overview_render_keys_mut(surface)
            .retain_pages(visible_pages)
        {
            self.update_render_cache_protection(cx);
        }
    }

    pub fn deactivate_render_cache(&mut self, cx: &mut Context<Self>) {
        self.viewport_render_keys.clear();
        self.thumbnail_sidebar_render_keys.clear();
        self.grid_overview_render_keys.clear();
        cx.global_mut::<RenderCache>()
            .clear_protected_renders(self.doc_id);
    }

    pub(super) fn request_render(&mut self, key: RenderKey, cx: &mut Context<Self>) {
        if !cx.global::<RenderCache>().contains_render(key) {
            self.start_background_render(key, cx);
        }
    }

    pub(super) fn cached_or_request_render(
        &mut self,
        key: RenderKey,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        self.cached_or_request_viewport_render(key, true, cx).0
    }

    /// Resolve a page image while optionally suppressing a new exact-size
    /// raster request. During a dock tween the page is painted from the
    /// nearest cached texture and scaled to the live bounds; once the tween
    /// finishes, the exact final raster is requested and replaces the fallback
    /// atomically. The returned key identifies the texture that was actually
    /// used so callers can protect it from cache eviction.
    pub(super) fn cached_or_request_viewport_render(
        &mut self,
        key: RenderKey,
        request_exact: bool,
        cx: &mut Context<Self>,
    ) -> (Option<Arc<RenderImage>>, Option<RenderKey>) {
        if let Some(image) = cx.global_mut::<RenderCache>().render(key) {
            return (Some(image), Some(key));
        }

        let has_pending_render = self.pending_renders.iter().any(|pending| {
            pending.key.page_idx == key.page_idx
                && pending.key.source_revision == key.source_revision
                && pending.key.rotation == key.rotation
                && pending.key.color_transform == key.color_transform
        });
        let has_failed_render = self.render_failed_for_key(key);
        let (page_width, _) = self.page_dimensions(key.page_idx);
        let render_scale = key.physical_width as f32 / page_width;
        let fallback = {
            let render_cache = cx.global_mut::<RenderCache>();
            render_cache.scaled_render_with_key(self.doc_id, key, render_scale)
        };

        // A cached image keeps motion visually continuous, so intermediate
        // exact-size work can wait. With no fallback we still bootstrap one
        // render; otherwise a document opened during motion could stay blank.
        if should_start_viewport_render(
            has_pending_render,
            has_failed_render,
            request_exact,
            fallback.is_some(),
        ) {
            self.start_background_render(key, cx);
        }

        if let Some((fallback_key, image)) = fallback {
            return (Some(image), Some(fallback_key));
        }

        let previous = self.previous_doc_id.and_then(|previous_doc_id| {
            cx.global_mut::<RenderCache>().scaled_render_with_key(
                previous_doc_id,
                key,
                render_scale,
            )
        });
        match previous {
            Some((fallback_key, image)) => (Some(image), Some(fallback_key)),
            None => (None, None),
        }
    }

    fn insert_cached_render(
        &mut self,
        key: RenderKey,
        image: Arc<RenderImage>,
        cx: &mut Context<Self>,
    ) {
        let (page_width, _) = self.page_dimensions(key.page_idx);
        let render_scale = key.physical_width as f32 / page_width;
        let removed_images = cx
            .global_mut::<RenderCache>()
            .insert_render(key, render_scale, image);
        for image in removed_images {
            cx.drop_image(image, None);
        }
    }

    fn render_failed_for_key(&self, key: RenderKey) -> bool {
        self.page_render_debug
            .get(&key.page_idx)
            .is_some_and(|attempt| attempt.key == key && !attempt.succeeded)
    }

    fn start_background_render(&mut self, key: RenderKey, cx: &mut Context<Self>) {
        if self.reload_task.is_some() || self.render_failed_for_key(key) {
            return;
        }
        if self.pending_renders.iter().any(|pending| {
            pending.key.page_idx == key.page_idx
                && pending.key.source_revision == key.source_revision
                && pending.key.rotation == key.rotation
                && pending.key.color_transform == key.color_transform
        }) {
            return;
        }
        self.pending_renders
            .retain(|pending| pending.key.page_idx != key.page_idx);
        if self.pending_renders.len() >= MAX_PENDING_PAGE_RENDERS {
            return;
        }

        let attempt_started_at = Instant::now();

        let page = &self.pages[key.page_idx];
        let page_bounds = page.bounds;
        let source_page_width = page.width;
        let source_page_height = page.height;
        let (page_width, page_height) = Self::page_dimensions_for(page, key.rotation);
        let cached_display_list = cx.global_mut::<RenderCache>().display_list(
            self.doc_id,
            key.page_idx,
            key.source_revision,
        );
        let (render_session, render_page_idx) = self
            .page_render_sources
            .get(&key.page_idx)
            .filter(|source| source.revision == key.source_revision)
            .map(|source| (source.render_session.clone(), 0))
            .unwrap_or_else(|| (self.render_session.clone(), key.page_idx));
        let background_executor = cx.background_executor().clone();

        let task = cx.spawn(async move |this, cx| {
            let (
                result,
                display_list_duration,
                display_list_cached,
                raster_duration,
                mupdf_draw,
                raster_bands,
            ) = cx
                .background_spawn(async move {
                    let (display_list, display_list_duration, display_list_cached) =
                        match cached_display_list {
                            Some(display_list) => (display_list, None, true),
                            None => {
                                let started_at = Instant::now();
                                match render_session
                                    .create_display_list(render_page_idx, page_bounds)
                                {
                                    Ok(display_list) => {
                                        (display_list, Some(started_at.elapsed()), false)
                                    }
                                    Err(error) => {
                                        return (
                                            Err(error),
                                            Some(started_at.elapsed()),
                                            false,
                                            None,
                                            None,
                                            None,
                                        );
                                    }
                                }
                            }
                        };
                    let raster_started_at = Instant::now();
                    let rendered_page = Self::render_page(
                        display_list.clone(),
                        page_bounds,
                        source_page_width,
                        source_page_height,
                        key,
                        background_executor,
                    )
                    .await;
                    let raster_duration = Some(raster_started_at.elapsed());
                    let (mupdf_draw, raster_bands) = rendered_page
                        .as_ref()
                        .map(|rendered| (Some(rendered.mupdf_draw), Some(rendered.raster_bands)))
                        .unwrap_or((None, None));
                    (
                        rendered_page.map(|rendered| (display_list, rendered.image)),
                        display_list_duration,
                        display_list_cached,
                        raster_duration,
                        mupdf_draw,
                        raster_bands,
                    )
                })
                .await;

            _ = this
                .update(cx, |this, cx| {
                    let Some(index) = this
                        .pending_renders
                        .iter()
                        .position(|pending| pending.key == key)
                    else {
                        return;
                    };
                    this.pending_renders.remove(index);

                    let succeeded = result.is_ok();
                    match result {
                        Ok((display_list, image)) => {
                            cx.global_mut::<RenderCache>().insert_display_list(
                                key.doc_id,
                                key.page_idx,
                                key.source_revision,
                                display_list,
                            );
                            let previous_image = this.take_previous_render_for_diff(key, cx);
                            this.insert_cached_render(key, image.clone(), cx);
                            if let Some(previous_image) = previous_image {
                                this.start_diff_flash_task(
                                    DiffRenderInput {
                                        key,
                                        page_bounds,
                                        page_width,
                                        page_height,
                                        rotation: key.rotation,
                                    },
                                    previous_image,
                                    image,
                                    cx,
                                );
                            }
                        }
                        Err(error) => eprintln!("failed to render PDF page: {error:#}"),
                    }
                    this.page_render_debug.insert(
                        key.page_idx,
                        PageRenderAttemptDebug {
                            key,
                            total: attempt_started_at.elapsed(),
                            display_list: display_list_duration,
                            display_list_cached,
                            raster: raster_duration,
                            mupdf_draw,
                            raster_bands,
                            succeeded,
                        },
                    );
                    // Same-key failures are suppressed until the render key
                    // changes or the document reloads, so notifying here can
                    // safely remove a pending-page loading indicator.
                    cx.notify();
                })
                .ok();
        });

        self.pending_renders
            .push(PendingRender { key, _task: task });
        cx.notify();
    }

    pub(super) fn get_or_create_display_list(
        &self,
        page_idx: usize,
        cx: &mut Context<Self>,
    ) -> Result<RetainedDisplayList> {
        if let Some(display_list) = cx.global_mut::<RenderCache>().display_list(
            self.doc_id,
            page_idx,
            self.page_render_sources
                .get(&page_idx)
                .map(|source| source.revision)
                .unwrap_or_default(),
        ) {
            return Ok(display_list);
        }

        let page_bounds = self
            .pages
            .get(page_idx)
            .ok_or_else(|| anyhow::anyhow!("PDF page index {page_idx} is out of bounds"))?
            .bounds;
        let (render_session, render_page_idx, source_revision) = self
            .page_render_sources
            .get(&page_idx)
            .map(|source| (source.render_session.clone(), 0, source.revision))
            .unwrap_or_else(|| (self.render_session.clone(), page_idx, 0));
        let display_list = render_session.create_display_list(render_page_idx, page_bounds)?;
        Ok(cx.global_mut::<RenderCache>().insert_display_list(
            self.doc_id,
            page_idx,
            source_revision,
            display_list,
        ))
    }

    fn take_previous_render_for_diff(
        &mut self,
        key: RenderKey,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        let previous_doc_id = self.previous_doc_id?;
        if !self.diff_pending_pages.remove(&key.page_idx) {
            return None;
        }

        let previous_key = RenderKey {
            doc_id: previous_doc_id,
            ..key
        };
        let (page_width, _) = self.page_dimensions(key.page_idx);
        let render_scale = key.physical_width as f32 / page_width;
        let render_cache = cx.global_mut::<RenderCache>();
        render_cache
            .render(previous_key)
            .or_else(|| render_cache.scaled_render(previous_doc_id, key, render_scale))
    }

    fn start_diff_flash_task(
        &mut self,
        input: DiffRenderInput,
        previous_image: Arc<RenderImage>,
        image: Arc<RenderImage>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let rects = cx
                .background_spawn(async move {
                    Self::diff_render_images(
                        &previous_image,
                        &image,
                        Rect::new(0.0, 0.0, input.page_width, input.page_height),
                        input.page_width,
                        input.page_height,
                        input.key.physical_width,
                        input.key.physical_height,
                    )
                })
                .await;

            if rects.is_empty() {
                return;
            }

            let update_result = this.update(cx, |this, cx| {
                let started_at = Instant::now();
                this.diff_flashes.extend(rects.into_iter().map(|rect| {
                    let rect = Self::view_rect_to_pdf(input.page_bounds, input.rotation, rect);
                    DiffFlash {
                        page_idx: input.key.page_idx,
                        rect,
                        started_at,
                    }
                }));
                this.arm_diff_flash_clear(cx);
                cx.notify();
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to show PDF reload diff flash: {error:#}");
            }
        })
        .detach();
    }

    #[ztracing::instrument(
        skip_all,
        fields(
            physical_width = key.physical_width,
            physical_height = key.physical_height
        )
    )]
    async fn render_page(
        display_list: RetainedDisplayList,
        page_bounds: Rect,
        source_page_width: f32,
        source_page_height: f32,
        key: RenderKey,
        background_executor: gpui::BackgroundExecutor,
    ) -> Result<RenderedPage> {
        let (page_width, page_height) =
            Self::rotated_dimensions(source_page_width, source_page_height, key.rotation);
        let zoom_x = key.physical_width as f32 / page_width;
        let zoom_y = key.physical_height as f32 / page_height;
        let matrix = Self::render_matrix(
            page_bounds,
            key.rotation,
            zoom_x,
            zoom_y,
            mupdf::Point { x: 0.0, y: 0.0 },
        );

        // Replaying ordinary display lists in bands adds scheduling and
        // stitching overhead. Restrict parallelism to large shade-bearing
        // rasters, where MuPDF's draw device can spend hundreds of
        // milliseconds evaluating a single shading on one thread.
        let candidate_band_count = raster_band_count(
            true,
            key.pixel_count(),
            key.physical_height,
            background_executor.num_cpus(),
        );
        let band_count = if candidate_band_count > 1 {
            match Self::display_list_has_shading(&display_list) {
                Ok(true) => candidate_band_count,
                Ok(false) => 1,
                Err(error) => {
                    eprintln!("failed to inspect PDF page shading; rendering one band: {error:#}");
                    1
                }
            }
        } else {
            1
        };

        let RasterizedSamples {
            mut samples,
            mupdf_draw,
            raster_bands,
        } = if band_count > 1 {
            Self::rasterize_page_bands(
                display_list.clone(),
                matrix.clone(),
                key.physical_width,
                key.physical_height,
                band_count,
                background_executor,
            )
            .await?
        } else {
            Self::rasterize_page_monolithic(
                &display_list,
                matrix.clone(),
                key.physical_width,
                key.physical_height,
            )?
        };

        if let Some(color_transform) = key.color_transform {
            let image_exclusions = Self::collect_images(&display_list)?
                .into_iter()
                .filter_map(|image| {
                    pixel_rect_for_transformed_bounds(
                        image.bounds,
                        &matrix,
                        key.physical_width as usize,
                        key.physical_height as usize,
                    )
                })
                .collect::<Vec<_>>();
            apply_pdf_duotone(
                &mut samples,
                key.physical_width as usize,
                key.physical_height as usize,
                color_transform,
                &image_exclusions,
            );
        }

        let mut bgra = Vec::with_capacity(samples.len());
        for pixel in samples.chunks_exact(4) {
            bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }

        let frame = Frame::new(
            RgbaImage::from_raw(key.physical_width, key.physical_height, bgra)
                .context("failed to create render image from pixmap")?,
        );

        Ok(RenderedPage {
            image: Arc::new(RenderImage::new(smallvec![frame])),
            mupdf_draw,
            raster_bands,
        })
    }

    pub(super) fn display_list_has_shading(display_list: &DisplayList) -> Result<bool> {
        let found = StdArc::new(AtomicBool::new(false));
        let device = Device::from_native(ShadeDetector {
            found: found.clone(),
        })?;
        display_list
            .run(&device, &Matrix::IDENTITY, Rect::INF)
            .context("failed to inspect PDF display list")?;
        Ok(found.load(std::sync::atomic::Ordering::Relaxed))
    }

    fn rgba_sample_len(width: u32, height: u32) -> Result<usize> {
        (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .context("PDF page raster dimensions overflow")
    }

    pub(super) fn rasterize_page_monolithic(
        display_list: &DisplayList,
        matrix: Matrix,
        width: u32,
        height: u32,
    ) -> Result<RasterizedSamples> {
        let width_i32 = i32::try_from(width).context("PDF page raster is too wide")?;
        let height_i32 = i32::try_from(height).context("PDF page raster is too tall")?;
        let mut pixmap =
            Pixmap::new_with_w_h(&Colorspace::device_rgb(), width_i32, height_i32, true)?;
        pixmap.samples_mut().fill(255);

        let device = Device::from_pixmap(&pixmap)?;
        let draw_started_at = Instant::now();
        display_list
            .run(&device, &matrix, Rect::INF)
            .context("failed to render PDF page")?;
        let mupdf_draw = draw_started_at.elapsed();
        drop(device);

        let expected_len = Self::rgba_sample_len(width, height)?;
        if pixmap.samples().len() != expected_len {
            bail!(
                "unexpected pixmap sample length: got {}, expected {}",
                pixmap.samples().len(),
                expected_len
            );
        }

        Ok(RasterizedSamples {
            samples: pixmap.samples().to_vec(),
            mupdf_draw,
            raster_bands: 1,
        })
    }

    pub(super) async fn rasterize_page_bands(
        display_list: RetainedDisplayList,
        matrix: Matrix,
        width: u32,
        height: u32,
        band_count: usize,
        background_executor: gpui::BackgroundExecutor,
    ) -> Result<RasterizedSamples> {
        let tasks = raster_band_ranges(height, band_count)
            .into_iter()
            .map(|range| {
                let display_list = display_list.clone();
                let matrix = matrix.clone();
                background_executor.spawn(async move {
                    Self::rasterize_page_band(display_list, matrix, width, range)
                })
            })
            .collect::<Vec<_>>();
        let mut bands = try_join_all(tasks)
            .await
            .context("failed to render PDF page bands")?;
        bands.sort_unstable_by_key(|band| band.range.y0);

        let draw_started_at = bands
            .iter()
            .map(|band| band.draw_started_at)
            .min()
            .context("PDF page band renderer produced no bands")?;
        let draw_finished_at = bands
            .iter()
            .map(|band| band.draw_finished_at)
            .max()
            .context("PDF page band renderer produced no bands")?;
        let raster_bands = bands.len();
        let mut samples = vec![255; Self::rgba_sample_len(width, height)?];
        let row_len = width as usize * 4;
        let mut next_y = 0u32;

        for band in bands {
            if band.range.y0 != next_y {
                bail!(
                    "PDF page bands are not contiguous: expected row {next_y}, got {}",
                    band.range.y0
                );
            }
            let expected_band_len = (band.range.y1 - band.range.y0) as usize * row_len;
            if band.samples.len() != expected_band_len {
                bail!(
                    "unexpected PDF band sample length: got {}, expected {}",
                    band.samples.len(),
                    expected_band_len
                );
            }
            let start = band.range.y0 as usize * row_len;
            let end = band.range.y1 as usize * row_len;
            samples[start..end].copy_from_slice(&band.samples);
            next_y = band.range.y1;
        }
        if next_y != height {
            bail!("PDF page bands ended at row {next_y}, expected {height}");
        }

        Ok(RasterizedSamples {
            samples,
            mupdf_draw: draw_finished_at.saturating_duration_since(draw_started_at),
            raster_bands,
        })
    }

    fn rasterize_page_band(
        display_list: RetainedDisplayList,
        matrix: Matrix,
        width: u32,
        range: RasterBandRange,
    ) -> Result<RasterBand> {
        let width_i32 = i32::try_from(width).context("PDF page raster is too wide")?;
        let y0 = i32::try_from(range.y0).context("PDF page band origin is too large")?;
        let band_height =
            i32::try_from(range.y1 - range.y0).context("PDF page band height is too large")?;
        let mut pixmap = Pixmap::new(
            &Colorspace::device_rgb(),
            0,
            y0,
            width_i32,
            band_height,
            true,
        )?;
        pixmap.samples_mut().fill(255);

        let device = Device::from_pixmap(&pixmap)?;
        let draw_started_at = Instant::now();
        display_list
            .run(&device, &matrix, Rect::INF)
            .context("failed to render PDF page band")?;
        let draw_finished_at = Instant::now();
        drop(device);

        let expected_len = Self::rgba_sample_len(width, range.y1 - range.y0)?;
        if pixmap.samples().len() != expected_len {
            bail!(
                "unexpected PDF band sample length: got {}, expected {}",
                pixmap.samples().len(),
                expected_len
            );
        }

        Ok(RasterBand {
            range,
            samples: pixmap.samples().to_vec(),
            draw_started_at,
            draw_finished_at,
        })
    }

    fn arm_diff_flash_clear(&mut self, cx: &mut Context<Self>) {
        self.diff_flash_clear_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DIFF_FLASH_DURATION).await;
            let update_result = this.update(cx, |this, cx| {
                this.diff_flashes.clear();
                this.diff_flash_clear_task = None;
                cx.notify();
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to clear PDF reload diff flash: {error:#}");
            }
        }));
    }

    fn diff_render_images(
        previous_image: &RenderImage,
        image: &RenderImage,
        page_bounds: Rect,
        page_width: f32,
        page_height: f32,
        physical_width: u32,
        physical_height: u32,
    ) -> Vec<Rect> {
        if physical_width == 0 || physical_height == 0 {
            return Vec::new();
        }

        let Some(expected_len) = (physical_width as usize)
            .checked_mul(physical_height as usize)
            .and_then(|len| len.checked_mul(4))
        else {
            return Vec::new();
        };
        let Some(previous_bytes) = previous_image.as_bytes(0) else {
            return Vec::new();
        };
        let Some(bytes) = image.as_bytes(0) else {
            return Vec::new();
        };
        if previous_bytes.len() != expected_len || bytes.len() != expected_len {
            return Vec::new();
        }

        let tile_size = DIFF_FLASH_TILE_SIZE.max(1);
        let cols = physical_width.div_ceil(tile_size);
        let rows = physical_height.div_ceil(tile_size);
        let Some(cell_count) = (cols as usize).checked_mul(rows as usize) else {
            return Vec::new();
        };
        let mut changed_cells = vec![false; cell_count];

        for row in 0..rows {
            for col in 0..cols {
                let x0 = col * tile_size;
                let y0 = row * tile_size;
                let x1 = ((col + 1) * tile_size).min(physical_width);
                let y1 = ((row + 1) * tile_size).min(physical_height);
                let area = (x1 - x0).saturating_mul(y1 - y0);
                let changed_threshold = (area / 100).max(2);
                let mut changed_pixels = 0_u32;

                'tile: for y in y0..y1 {
                    for x in x0..x1 {
                        let offset = ((y as usize * physical_width as usize) + x as usize) * 4;
                        let delta = u16::from(previous_bytes[offset].abs_diff(bytes[offset]))
                            + u16::from(previous_bytes[offset + 1].abs_diff(bytes[offset + 1]))
                            + u16::from(previous_bytes[offset + 2].abs_diff(bytes[offset + 2]));
                        if delta > DIFF_FLASH_PIXEL_THRESHOLD {
                            changed_pixels += 1;
                            if changed_pixels >= changed_threshold {
                                changed_cells[(row as usize * cols as usize) + col as usize] = true;
                                break 'tile;
                            }
                        }
                    }
                }
            }
        }

        let mut runs = Vec::new();
        for row in 0..rows {
            let mut col = 0;
            while col < cols {
                if !changed_cells[(row as usize * cols as usize) + col as usize] {
                    col += 1;
                    continue;
                }

                let start_col = col;
                while col < cols && changed_cells[(row as usize * cols as usize) + col as usize] {
                    col += 1;
                }
                let end_col = col;

                if let Some((last_x0, _, last_x1, last_y1)) = runs.last_mut()
                    && *last_x0 == start_col
                    && *last_x1 == end_col
                    && *last_y1 == row
                {
                    *last_y1 = row + 1;
                } else {
                    runs.push((start_col, row, end_col, row + 1));
                }
            }
        }

        if runs.len() > DIFF_FLASH_MAX_RECTS {
            let mut min_x = u32::MAX;
            let mut min_y = u32::MAX;
            let mut max_x = 0;
            let mut max_y = 0;
            for (x0, y0, x1, y1) in runs {
                min_x = min_x.min(x0);
                min_y = min_y.min(y0);
                max_x = max_x.max(x1);
                max_y = max_y.max(y1);
            }
            runs = vec![(min_x, min_y, max_x, max_y)];
        }

        let scale_x = page_width / physical_width as f32;
        let scale_y = page_height / physical_height as f32;
        runs.into_iter()
            .filter_map(|(cell_x0, cell_y0, cell_x1, cell_y1)| {
                let pixel_x0 = (cell_x0 * tile_size).min(physical_width);
                let pixel_y0 = (cell_y0 * tile_size).min(physical_height);
                let pixel_x1 = (cell_x1 * tile_size).min(physical_width);
                let pixel_y1 = (cell_y1 * tile_size).min(physical_height);
                if pixel_x1 <= pixel_x0 || pixel_y1 <= pixel_y0 {
                    return None;
                }

                Some(Rect::new(
                    page_bounds.x0 + pixel_x0 as f32 * scale_x,
                    page_bounds.y0 + pixel_y0 as f32 * scale_y,
                    page_bounds.x0 + pixel_x1 as f32 * scale_x,
                    page_bounds.y0 + pixel_y1 as f32 * scale_y,
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod document_color_tests {
    use super::*;

    #[test]
    fn duotone_maps_black_and_white_and_preserves_embedded_images() {
        let mut samples = vec![
            0, 0, 0, 255, // black vector content
            255, 255, 255, 255, // white page
            12, 34, 56, 127, // embedded image pixel
        ];
        let transform = PdfColorTransform {
            background: 0x102030,
            foreground: 0xe0d0c0,
        };

        apply_pdf_duotone(
            &mut samples,
            3,
            1,
            transform,
            &[PixelRect {
                x0: 2,
                y0: 0,
                x1: 3,
                y1: 1,
            }],
        );

        assert_eq!(&samples[0..4], &[0xe0, 0xd0, 0xc0, 255]);
        assert_eq!(&samples[4..8], &[0x10, 0x20, 0x30, 255]);
        assert_eq!(&samples[8..12], &[12, 34, 56, 127]);
    }

    #[test]
    fn transformed_image_bounds_are_clipped_to_the_pixmap() {
        let rect = pixel_rect_for_transformed_bounds(
            Rect::new(-5.0, 2.0, 8.2, 12.8),
            &Matrix::IDENTITY,
            10,
            10,
        );

        assert_eq!(
            rect,
            Some(PixelRect {
                x0: 0,
                y0: 2,
                x1: 9,
                y1: 10,
            })
        );
    }
}
