use super::*;

impl PdfView {
    pub(super) fn copy_image(&mut self, cx: &mut Context<Self>) {
        let Some((page_idx, image_idx)) = self.context_menu_image else {
            return;
        };
        if page_idx >= self.pages.len() {
            return;
        };
        let display_list = match self.get_or_create_display_list(page_idx, cx) {
            Ok(display_list) => display_list,
            Err(error) => {
                eprintln!("failed to load PDF page for image copy: {error:#}");
                return;
            }
        };

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { Self::copy_image_to_clipboard(display_list, image_idx) },
                )
                .await;

            if let Err(error) = this.update(cx, |this, _| match result {
                Ok(clipboard) => {
                    this._image_clipboard = Some(clipboard);
                }
                Err(error) => eprintln!("failed to copy PDF image: {error:#}"),
            }) {
                eprintln!("failed to update PDF image clipboard state: {error:#}");
            }
        })
        .detach();
    }

    fn copy_image_to_clipboard(
        display_list: RetainedDisplayList,
        image_idx: usize,
    ) -> Result<Clipboard> {
        let (width, height, rgba) =
            Self::rasterize_image_from_display_list(&display_list, image_idx)?;

        let mut clipboard = Clipboard::new().context("failed to open clipboard")?;
        clipboard
            .set_image(ImageData {
                width,
                height,
                bytes: Cow::Owned(rgba),
            })
            .context("failed to write image to clipboard")?;

        Ok(clipboard)
    }

    pub(super) fn save_image(&mut self, cx: &mut Context<Self>) {
        let Some((page_idx, image_idx)) = self.context_menu_image else {
            return;
        };
        if page_idx >= self.pages.len() {
            return;
        };
        let display_list = match self.get_or_create_display_list(page_idx, cx) {
            Ok(display_list) => display_list,
            Err(error) => {
                eprintln!("failed to load PDF page for image save: {error:#}");
                return;
            }
        };
        let suggested_name = format!("page-{}-image-{}.png", page_idx + 1, image_idx + 1);
        let directory = dirs::picture_dir()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |_, cx| match dialog.await {
            Ok(Ok(Some(path))) => {
                let result = cx
                    .background_spawn(async move {
                        Self::save_image_to_path(display_list, image_idx, path)
                    })
                    .await;

                if let Err(error) = result {
                    eprintln!("failed to save PDF image: {error:#}");
                }
            }
            Ok(Err(error)) => eprintln!("failed to open save image dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();
    }

    fn save_image_to_path(
        display_list: RetainedDisplayList,
        image_idx: usize,
        path: PathBuf,
    ) -> Result<()> {
        let (width, height, rgba) =
            Self::rasterize_image_from_display_list(&display_list, image_idx)?;
        let image = RgbaImage::from_raw(width as u32, height as u32, rgba)
            .context("failed to create image data")?;
        image
            .save_with_format(&path, image::ImageFormat::Png)
            .with_context(|| format!("failed to save image to {}", path.display()))
    }

    pub(super) fn save_page_as_image(&mut self, cx: &mut Context<Self>) {
        let Some(page_idx) = self.context_menu_page else {
            return;
        };
        let Some(page) = self.pages.get(page_idx) else {
            return;
        };
        let page_bounds = page.bounds;
        let page_width = page.width;
        let page_height = page.height;
        let rotation = self.rotation;
        let display_list = match self.get_or_create_display_list(page_idx, cx) {
            Ok(display_list) => display_list,
            Err(error) => {
                eprintln!("failed to load PDF page for image export: {error:#}");
                return;
            }
        };
        let suggested_name = format!("page-{}.png", page_idx + 1);
        let directory = dirs::picture_dir()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |_, cx| match dialog.await {
            Ok(Ok(Some(path))) => {
                let result = cx
                    .background_spawn(async move {
                        Self::save_page_to_path(
                            display_list,
                            page_bounds,
                            page_width,
                            page_height,
                            rotation,
                            path,
                        )
                    })
                    .await;

                if let Err(error) = result {
                    eprintln!("failed to save PDF page as image: {error:#}");
                }
            }
            Ok(Err(error)) => eprintln!("failed to open save page image dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();
    }

    fn region_image_exports(
        &mut self,
        reference: &RegionImageReference,
        cx: &mut Context<Self>,
    ) -> Result<Vec<RegionImageExport>> {
        reference
            .regions
            .iter()
            .map(|region| {
                let page_idx = region.page.0;
                if page_idx >= self.pages.len() {
                    bail!("selected region references a missing PDF page");
                }
                Ok(RegionImageExport {
                    display_list: self.get_or_create_display_list(page_idx, cx)?,
                    page_bounds: self.pages[page_idx].bounds,
                    region: *region,
                    rotation: self.rotation,
                })
            })
            .collect()
    }

    pub(super) fn copy_region(&mut self, cx: &mut Context<Self>) {
        let Some(reference) = self.selected_region.clone() else {
            return;
        };
        let exports = match self.region_image_exports(&reference, cx) {
            Ok(exports) => exports,
            Err(error) => {
                eprintln!("failed to prepare PDF region copy: {error:#}");
                return;
            }
        };

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let (width, height, rgba) =
                        Self::rasterize_region_stack_to_rgba(&exports, PAGE_EXPORT_SCALE)?;
                    let mut clipboard =
                        Clipboard::new().context("failed to open clipboard for region copy")?;
                    clipboard
                        .set_image(ImageData {
                            width: width as usize,
                            height: height as usize,
                            bytes: Cow::Owned(rgba),
                        })
                        .context("failed to write PDF region image to clipboard")?;
                    Ok::<_, anyhow::Error>(clipboard)
                })
                .await;

            if let Err(error) = this.update(cx, |this, cx| match result {
                Ok(clipboard) => {
                    this._image_clipboard = Some(clipboard);
                    if clear_copied_region_selection(
                        &mut this.selected_region,
                        &mut this.context_menu_region,
                        &reference,
                    ) {
                        cx.notify();
                    }
                }
                Err(error) => eprintln!("failed to copy PDF region: {error:#}"),
            }) {
                eprintln!("failed to update PDF region clipboard state: {error:#}");
            }
        })
        .detach();
    }

    pub(super) fn save_region(&mut self, cx: &mut Context<Self>) {
        let Some(reference) = self.selected_region.clone() else {
            return;
        };
        let exports = match self.region_image_exports(&reference, cx) {
            Ok(exports) => exports,
            Err(error) => {
                eprintln!("failed to prepare PDF region save: {error:#}");
                return;
            }
        };
        let first_page = reference
            .regions
            .first()
            .map_or(1, |region| region.page.0 + 1);
        let last_page = reference
            .regions
            .last()
            .map_or(first_page, |region| region.page.0 + 1);
        let suggested_name = if first_page == last_page {
            format!("region-page-{first_page}.png")
        } else {
            format!("region-pages-{first_page}-{last_page}.png")
        };
        let directory = dirs::picture_dir()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |_, cx| match dialog.await {
            Ok(Ok(Some(path))) => {
                let result = cx
                    .background_spawn(async move {
                        let (width, height, rgba) =
                            Self::rasterize_region_stack_to_rgba(&exports, PAGE_EXPORT_SCALE)?;
                        let image = RgbaImage::from_raw(width, height, rgba)
                            .context("failed to create region image data")?;
                        image
                            .save_with_format(&path, image::ImageFormat::Png)
                            .with_context(|| {
                                format!("failed to save region image to {}", path.display())
                            })
                    })
                    .await;
                if let Err(error) = result {
                    eprintln!("failed to save PDF region: {error:#}");
                }
            }
            Ok(Err(error)) => eprintln!("failed to open save region dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();
    }

    pub(super) fn rasterize_region_stack_to_rgba<L: AsRef<DisplayList>>(
        exports: &[RegionImageExport<L>],
        scale: f32,
    ) -> Result<(u32, u32, Vec<u8>)> {
        if exports.is_empty() || !scale.is_finite() || scale <= 0.0 {
            bail!("PDF image region is empty");
        }

        let mut dimensions = Vec::with_capacity(exports.len());
        let mut max_width = 1u32;
        let mut total_height = 0u32;
        for export in exports {
            let view_region = export.view_region();
            let width = ((view_region.x1 - view_region.x0) * scale).ceil().max(1.0);
            let height = ((view_region.y1 - view_region.y0) * scale).ceil().max(1.0);
            if width > i32::MAX as f32 || height > i32::MAX as f32 {
                bail!("PDF image region is too large");
            }
            let width = width as u32;
            let height = height as u32;
            max_width = max_width.max(width);
            total_height = total_height
                .checked_add(height)
                .context("combined region image is too tall")?;
            dimensions.push((width, height));
        }

        let total_pixels = (max_width as u64)
            .checked_mul(total_height as u64)
            .context("combined region image is too large")?;
        if total_pixels > MAX_REGION_EXPORT_PIXELS {
            bail!(
                "combined region image is too large: {} megapixels exceeds the {} megapixel limit",
                total_pixels.div_ceil(1_000_000),
                MAX_REGION_EXPORT_PIXELS / 1_000_000
            );
        }

        let mut image =
            RgbaImage::from_pixel(max_width, total_height, image::Rgba([255, 255, 255, 255]));
        let mut y_offset = 0;
        for (export, (width, height)) in exports.iter().zip(dimensions) {
            let (_, _, rgba) = Self::rasterize_pdf_region_to_rgba(export, scale)?;
            let crop = RgbaImage::from_raw(width, height, rgba)
                .context("failed to create region crop image data")?;
            let x_offset = (max_width - width) / 2;
            for y in 0..height {
                for x in 0..width {
                    image.put_pixel(x_offset + x, y_offset + y, *crop.get_pixel(x, y));
                }
            }
            y_offset = y_offset
                .checked_add(height)
                .context("combined region image is too tall")?;
        }

        Ok((max_width, total_height, image.into_raw()))
    }

    fn rasterize_pdf_region_to_rgba<L: AsRef<DisplayList>>(
        export: &RegionImageExport<L>,
        scale: f32,
    ) -> Result<(u32, u32, Vec<u8>)> {
        let view_region = export.view_region();
        let width = ((view_region.x1 - view_region.x0) * scale).ceil().max(1.0);
        let height = ((view_region.y1 - view_region.y0) * scale).ceil().max(1.0);
        if width > i32::MAX as f32 || height > i32::MAX as f32 {
            bail!("PDF image region is too large");
        }
        let width = width as i32;
        let height = height as i32;
        let mut pixmap = Pixmap::new_with_w_h(&Colorspace::device_rgb(), width, height, true)?;
        pixmap.samples_mut().fill(255);
        let matrix = Self::render_matrix(
            export.page_bounds,
            export.rotation,
            scale,
            scale,
            mupdf::Point {
                x: view_region.x0,
                y: view_region.y0,
            },
        );
        {
            let device = Device::from_pixmap(&pixmap)?;
            export
                .display_list
                .as_ref()
                .run(&device, &matrix, Rect::INF)
                .context("failed to rasterize PDF image region")?;
        }
        let width = pixmap.width();
        let height = pixmap.height();
        let samples = pixmap.samples();
        let expected_len = width as usize * height as usize * 4;
        if samples.len() != expected_len || pixmap.n() != 4 {
            bail!(
                "unexpected region image pixmap layout: {} samples, {} components",
                samples.len(),
                pixmap.n()
            );
        }
        Ok((width, height, samples.to_vec()))
    }

    pub(super) fn render_region_jpeg<L: AsRef<DisplayList>>(
        exports: &[RegionImageExport<L>],
        quality: PageImageQuality,
    ) -> Result<Vec<u8>> {
        if exports.is_empty() {
            bail!("PDF image region is empty");
        }
        let max_width = exports
            .iter()
            .map(|export| export.view_region())
            .map(|rect| rect.x1 - rect.x0)
            .fold(0.0_f32, f32::max);
        let total_height = exports
            .iter()
            .map(|export| export.view_region())
            .map(|rect| rect.y1 - rect.y0)
            .sum::<f32>();
        let scale = quality.max_dimension() as f32 / max_width.max(total_height).max(1.0);
        let (width, height, rgba) = Self::rasterize_region_stack_to_rgba(exports, scale)?;
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for pixel in rgba.chunks_exact(4) {
            rgb.extend_from_slice(&pixel[..3]);
        }
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, quality.jpeg_quality())
            .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
            .context("failed to encode PDF image region as JPEG")?;
        Ok(bytes)
    }

    pub(super) fn export_selected_pages_as_text(
        &mut self,
        source: PageSelectionSource,
        cx: &mut Context<Self>,
    ) {
        let page_indices = self.selected_page_indices(source);
        if page_indices.is_empty() {
            return;
        }

        let mut export_pages = Vec::with_capacity(page_indices.len());
        for page_idx in page_indices.iter().copied() {
            let display_list = match self.get_or_create_display_list(page_idx, cx) {
                Ok(display_list) => display_list,
                Err(error) => {
                    eprintln!("failed to load PDF page for text export: {error:#}");
                    return;
                }
            };
            export_pages.push((page_idx, display_list));
        }

        let suggested_name = if export_pages.len() == 1 {
            format!("page-{}.txt", export_pages[0].0 + 1)
        } else {
            "selected-pages.txt".to_string()
        };
        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |_, cx| match dialog.await {
            Ok(Ok(Some(path))) => {
                let result = cx
                    .background_spawn(async move {
                        Self::save_selected_pages_text_to_path(export_pages, path)
                    })
                    .await;

                if let Err(error) = result {
                    eprintln!("failed to export selected PDF pages as text: {error:#}");
                }
            }
            Ok(Err(error)) => eprintln!("failed to open text export dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();

        self.cancel_page_selection(source, cx);
    }

    pub(super) fn export_selected_pages_as_image(
        &mut self,
        source: PageSelectionSource,
        cx: &mut Context<Self>,
    ) {
        let page_indices = self.selected_page_indices(source);
        if page_indices.is_empty() {
            return;
        }

        let mut export_pages = Vec::with_capacity(page_indices.len());
        let rotation = self.rotation;
        for page_idx in page_indices {
            let Some(page) = self.pages.get(page_idx) else {
                continue;
            };
            let bounds = page.bounds;
            let width = page.width;
            let height = page.height;
            let display_list = match self.get_or_create_display_list(page_idx, cx) {
                Ok(display_list) => display_list,
                Err(error) => {
                    eprintln!("failed to load PDF page for image export: {error:#}");
                    return;
                }
            };
            export_pages.push(PageImageExport {
                page_idx,
                display_list,
                bounds,
                width,
                height,
                rotation,
            });
        }

        if export_pages.is_empty() {
            return;
        }

        let suggested_name = if export_pages.len() == 1 {
            format!("page-{}.png", export_pages[0].page_idx + 1)
        } else {
            "selected-pages.png".to_string()
        };
        let directory = dirs::picture_dir()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |_, cx| match dialog.await {
            Ok(Ok(Some(path))) => {
                let result = cx
                    .background_spawn(async move {
                        Self::save_selected_pages_image_to_path(export_pages, path)
                    })
                    .await;

                if let Err(error) = result {
                    eprintln!("failed to export selected PDF pages as image: {error:#}");
                }
            }
            Ok(Err(error)) => eprintln!("failed to open image export dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();

        self.cancel_page_selection(source, cx);
    }

    fn save_selected_pages_text_to_path(
        pages: Vec<(usize, RetainedDisplayList)>,
        path: PathBuf,
    ) -> Result<()> {
        let mut text = String::new();
        for (page_ix, (_, display_list)) in pages.into_iter().enumerate() {
            if page_ix > 0 {
                text.push_str("\n\n");
            }
            text.push_str(&Self::text_from_display_list(display_list.as_ref())?);
        }

        fs::write(&path, text).with_context(|| format!("failed to save text to {}", path.display()))
    }

    fn text_from_display_list(display_list: &DisplayList) -> Result<String> {
        let mut glyphs = Vec::new();
        let mut text_lines = Vec::new();
        let mut next_line_idx = 0;
        let mut next_block_idx = 0;
        let range = Self::extract_glyphs(
            display_list,
            0,
            &mut glyphs,
            &mut text_lines,
            &mut next_line_idx,
            &mut next_block_idx,
        )?;
        Ok(Self::text_for_glyph_range(&glyphs, range).unwrap_or_default())
    }

    fn save_selected_pages_image_to_path(pages: Vec<PageImageExport>, path: PathBuf) -> Result<()> {
        if pages.is_empty() {
            bail!("no pages selected for image export");
        }

        let mut rendered_pages = Vec::with_capacity(pages.len());
        let mut max_width = 1u32;
        let mut total_height = 0u32;

        for page in pages {
            let (width, height, rgba) = Self::rasterize_page_to_rgba(
                page.display_list.as_ref(),
                page.bounds,
                page.width,
                page.height,
                page.rotation,
            )?;
            max_width = max_width.max(width);
            total_height = total_height
                .checked_add(height)
                .context("combined page image is too tall")?;
            rendered_pages.push((width, height, rgba));
        }

        let total_bytes = (max_width as u64)
            .checked_mul(total_height as u64)
            .and_then(|pixels| pixels.checked_mul(4))
            .context("combined page image is too large")?;
        if total_bytes > usize::MAX as u64 {
            bail!("combined page image is too large");
        }

        let mut image =
            RgbaImage::from_pixel(max_width, total_height, image::Rgba([255, 255, 255, 255]));
        let mut y_offset = 0u32;
        for (width, height, rgba) in rendered_pages {
            let page_image = RgbaImage::from_raw(width, height, rgba)
                .context("failed to create page image data")?;
            let x_offset = (max_width - width) / 2;
            for y in 0..height {
                for x in 0..width {
                    let pixel = *page_image.get_pixel(x, y);
                    image.put_pixel(x_offset + x, y_offset + y, pixel);
                }
            }
            y_offset = y_offset
                .checked_add(height)
                .context("combined page image is too tall")?;
        }

        image
            .save_with_format(&path, image::ImageFormat::Png)
            .with_context(|| format!("failed to save image to {}", path.display()))
    }

    fn save_page_to_path(
        display_list: RetainedDisplayList,
        page_bounds: Rect,
        page_width: f32,
        page_height: f32,
        rotation: DocumentRotation,
        path: PathBuf,
    ) -> Result<()> {
        let (width, height, rgba) = Self::rasterize_page_to_rgba(
            &display_list,
            page_bounds,
            page_width,
            page_height,
            rotation,
        )?;
        let image =
            RgbaImage::from_raw(width, height, rgba).context("failed to create page image data")?;
        image
            .save_with_format(&path, image::ImageFormat::Png)
            .with_context(|| format!("failed to save image to {}", path.display()))
    }

    #[ztracing::instrument(skip_all)]
    fn rasterize_page_to_rgba(
        display_list: &DisplayList,
        page_bounds: Rect,
        page_width: f32,
        page_height: f32,
        rotation: DocumentRotation,
    ) -> Result<(u32, u32, Vec<u8>)> {
        let (display_width, display_height) =
            Self::rotated_dimensions(page_width, page_height, rotation);
        let physical_width = (display_width * PAGE_EXPORT_SCALE).ceil().max(1.0);
        let physical_height = (display_height * PAGE_EXPORT_SCALE).ceil().max(1.0);
        if physical_width > i32::MAX as f32 || physical_height > i32::MAX as f32 {
            bail!("PDF page is too large to export as an image");
        }

        let physical_width_i32 = physical_width as i32;
        let physical_height_i32 = physical_height as i32;
        let mut pixmap = Pixmap::new_with_w_h(
            &Colorspace::device_rgb(),
            physical_width_i32,
            physical_height_i32,
            true,
        )?;
        pixmap.samples_mut().fill(255);

        let zoom_x = physical_width / display_width.max(1.0);
        let zoom_y = physical_height / display_height.max(1.0);
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
                .context("failed to render PDF page image")?;
        }

        let width = pixmap.width();
        let height = pixmap.height();
        let samples = pixmap.samples();
        let expected_len = width as usize * height as usize * 4;
        if samples.len() != expected_len || pixmap.n() != 4 {
            bail!(
                "unexpected page image pixmap layout: {} samples, {} components",
                samples.len(),
                pixmap.n()
            );
        }

        Ok((width, height, samples.to_vec()))
    }

    fn rasterize_image_from_display_list(
        display_list: &DisplayList,
        image_idx: usize,
    ) -> Result<(usize, usize, Vec<u8>)> {
        let image = Self::collect_images(display_list)?
            .into_iter()
            .nth(image_idx)
            .ok_or_else(|| anyhow!("PDF image no longer exists"))?;

        Self::rasterize_image_to_rgba(&image.image)
    }

    fn rasterize_image_to_rgba(image: &PdfImage) -> Result<(usize, usize, Vec<u8>)> {
        let width = image.width();
        let height = image.height();
        if width == 0 || height == 0 {
            bail!("PDF image has zero dimensions");
        }
        if width > i32::MAX as u32 || height > i32::MAX as u32 {
            bail!("PDF image is too large to copy");
        }

        let mut pixmap =
            Pixmap::new_with_w_h(&Colorspace::device_rgb(), width as i32, height as i32, true)?;
        pixmap.clear()?;

        let matrix = Matrix::new(width as f32, 0.0, 0.0, height as f32, 0.0, 0.0);
        {
            let device = Device::from_pixmap(&pixmap)?;
            device
                .fill_image(image, &matrix, 1.0, ColorParams::default())
                .context("failed to rasterize PDF image")?;
        }

        let samples = pixmap.samples();
        let expected_len = width as usize * height as usize * 4;
        if samples.len() != expected_len || pixmap.n() != 4 {
            bail!(
                "unexpected image pixmap layout: {} samples, {} components",
                samples.len(),
                pixmap.n()
            );
        }

        let mut rgba = samples.to_vec();
        Self::unpremultiply_rgba(&mut rgba);
        Ok((width as usize, height as usize, rgba))
    }

    fn unpremultiply_rgba(rgba: &mut [u8]) {
        for pixel in rgba.chunks_exact_mut(4) {
            let alpha = pixel[3] as u32;
            if alpha == 0 {
                pixel[0] = 0;
                pixel[1] = 0;
                pixel[2] = 0;
            } else if alpha < 255 {
                pixel[0] = ((pixel[0] as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                pixel[1] = ((pixel[1] as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                pixel[2] = ((pixel[2] as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}
