use super::*;

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
        if let Some(image) = cx.global_mut::<RenderCache>().render(key) {
            return Some(image);
        }

        if !self.pending_renders.iter().any(|pending| {
            pending.key.page_idx == key.page_idx && pending.key.rotation == key.rotation
        }) {
            self.start_background_render(key, cx);
        }
        let (page_width, _) = self.page_dimensions(key.page_idx);
        let render_scale = key.physical_width as f32 / page_width;
        let render_cache = cx.global_mut::<RenderCache>();
        if let Some(image) =
            render_cache.scaled_render(self.doc_id, key.page_idx, key.rotation, render_scale)
        {
            return Some(image);
        }

        self.previous_doc_id.and_then(|previous_doc_id| {
            render_cache.scaled_render(previous_doc_id, key.page_idx, key.rotation, render_scale)
        })
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
            pending.key.page_idx == key.page_idx && pending.key.rotation == key.rotation
        }) {
            return;
        }
        self.pending_renders
            .retain(|pending| pending.key.page_idx != key.page_idx);
        if self.pending_renders.len() >= MAX_PENDING_PAGE_RENDERS {
            return;
        }

        let page = &self.pages[key.page_idx];
        let page_bounds = page.bounds;
        let source_page_width = page.width;
        let source_page_height = page.height;
        let (page_width, page_height) = Self::page_dimensions_for(page, key.rotation);
        let cached_display_list = cx
            .global_mut::<RenderCache>()
            .display_list(self.doc_id, key.page_idx);
        let render_session = self.render_session.clone();

        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let display_list = match cached_display_list {
                        Some(display_list) => display_list,
                        None => render_session.create_display_list(key.page_idx, page_bounds)?,
                    };
                    let image = Self::render_page(
                        &display_list,
                        page_bounds,
                        source_page_width,
                        source_page_height,
                        key.rotation,
                        key.physical_width,
                        key.physical_height,
                    )?;
                    Ok::<_, anyhow::Error>((display_list, image))
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

                    match result {
                        Ok((display_list, image)) => {
                            cx.global_mut::<RenderCache>().insert_display_list(
                                key.doc_id,
                                key.page_idx,
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
                            cx.notify();
                        }
                        Err(error) => eprintln!("failed to render PDF page: {error:#}"),
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
        if let Some(display_list) = cx
            .global_mut::<RenderCache>()
            .display_list(self.doc_id, page_idx)
        {
            return Ok(display_list);
        }

        let page_bounds = self
            .pages
            .get(page_idx)
            .ok_or_else(|| anyhow::anyhow!("PDF page index {page_idx} is out of bounds"))?
            .bounds;
        let display_list = self
            .render_session
            .create_display_list(page_idx, page_bounds)?;
        Ok(cx
            .global_mut::<RenderCache>()
            .insert_display_list(self.doc_id, page_idx, display_list))
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
        render_cache.render(previous_key).or_else(|| {
            render_cache.scaled_render(previous_doc_id, key.page_idx, key.rotation, render_scale)
        })
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

    #[ztracing::instrument(skip_all, fields(physical_width, physical_height))]
    fn render_page(
        display_list: &DisplayList,
        page_bounds: Rect,
        source_page_width: f32,
        source_page_height: f32,
        rotation: DocumentRotation,
        physical_width: u32,
        physical_height: u32,
    ) -> Result<Arc<RenderImage>> {
        let (page_width, page_height) =
            Self::rotated_dimensions(source_page_width, source_page_height, rotation);
        let zoom_x = physical_width as f32 / page_width;
        let zoom_y = physical_height as f32 / page_height;
        let physical_width = physical_width as i32;
        let physical_height = physical_height as i32;

        let mut pixmap = Pixmap::new_with_w_h(
            &Colorspace::device_rgb(),
            physical_width,
            physical_height,
            true,
        )?;
        pixmap.samples_mut().fill(255);

        let matrix = Self::render_matrix(
            page_bounds,
            rotation,
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
        let samples = pixmap.samples();
        let expected_len = width as usize * height as usize * 4;
        if samples.len() != expected_len {
            bail!(
                "unexpected pixmap sample length: got {}, expected {}",
                samples.len(),
                expected_len
            );
        }

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
