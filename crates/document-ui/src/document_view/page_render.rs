use super::*;

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
    request_exact: bool,
    has_fallback: bool,
) -> bool {
    !has_pending_render && (request_exact || !has_fallback)
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
        let (page_width, _) = self.page_dimensions(key.page_idx);
        let render_scale = key.physical_width as f32 / page_width;
        let fallback = {
            let render_cache = cx.global_mut::<RenderCache>();
            render_cache.scaled_render_with_key(self.doc_id, key, render_scale)
        };

        // A cached image keeps motion visually continuous, so intermediate
        // exact-size work can wait. With no fallback we still bootstrap one
        // render; otherwise a document opened during motion could stay blank.
        if should_start_viewport_render(has_pending_render, request_exact, fallback.is_some()) {
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

    fn start_background_render(&mut self, key: RenderKey, cx: &mut Context<Self>) {
        if self.reload_task.is_some() {
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

        let task = cx.spawn(async move |this, cx| {
            let (result, display_list_duration, display_list_cached, raster_duration) = cx
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
                                            Err(error.into()),
                                            Some(started_at.elapsed()),
                                            false,
                                            None,
                                        );
                                    }
                                }
                            }
                        };
                    let raster_started_at = Instant::now();
                    let image = Self::render_page(
                        &display_list,
                        page_bounds,
                        source_page_width,
                        source_page_height,
                        key,
                    );
                    let raster_duration = Some(raster_started_at.elapsed());
                    (
                        image.map(|image| (display_list, image)),
                        display_list_duration,
                        display_list_cached,
                        raster_duration,
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
                            succeeded,
                        },
                    );
                    // A successful texture needs a repaint. A failure is
                    // observed by the independently refreshed debug HUD; do
                    // not notify the document here, because repainting an
                    // empty page would immediately enqueue the same failing
                    // render again in a tight loop.
                    if succeeded {
                        cx.notify();
                    }
                })
                .ok();
        });

        self.pending_renders
            .push(PendingRender { key, _task: task });
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
    fn render_page(
        display_list: &DisplayList,
        page_bounds: Rect,
        source_page_width: f32,
        source_page_height: f32,
        key: RenderKey,
    ) -> Result<Arc<RenderImage>> {
        let (page_width, page_height) =
            Self::rotated_dimensions(source_page_width, source_page_height, key.rotation);
        let zoom_x = key.physical_width as f32 / page_width;
        let zoom_y = key.physical_height as f32 / page_height;
        let physical_width = key.physical_width as i32;
        let physical_height = key.physical_height as i32;

        let mut pixmap = Pixmap::new_with_w_h(
            &Colorspace::device_rgb(),
            physical_width,
            physical_height,
            true,
        )?;
        pixmap.samples_mut().fill(255);

        let matrix = Self::render_matrix(
            page_bounds,
            key.rotation,
            zoom_x,
            zoom_y,
            mupdf::Point { x: 0.0, y: 0.0 },
        );
        {
            let device = Device::from_pixmap(&pixmap)?;
            display_list
                .run(&device, &matrix, Rect::INF)
                .context("failed to render PDF page")?;
        }

        let width = pixmap.width();
        let height = pixmap.height();
        let expected_len = width as usize * height as usize * 4;
        if pixmap.samples().len() != expected_len {
            bail!(
                "unexpected pixmap sample length: got {}, expected {}",
                pixmap.samples().len(),
                expected_len
            );
        }

        if let Some(color_transform) = key.color_transform {
            let image_exclusions = Self::collect_images(display_list)?
                .into_iter()
                .filter_map(|image| {
                    pixel_rect_for_transformed_bounds(
                        image.bounds,
                        &matrix,
                        width as usize,
                        height as usize,
                    )
                })
                .collect::<Vec<_>>();
            apply_pdf_duotone(
                pixmap.samples_mut(),
                width as usize,
                height as usize,
                color_transform,
                &image_exclusions,
            );
        }

        let samples = pixmap.samples();
        let mut bgra = Vec::with_capacity(samples.len());
        for pixel in samples.chunks_exact(4) {
            bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }

        let frame = Frame::new(
            RgbaImage::from_raw(width, height, bgra)
                .context("failed to create render image from pixmap")?,
        );

        Ok(Arc::new(RenderImage::new(smallvec![frame])))
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
