//! Cover thumbnail fingerprinting, rendering, persistence, and view updates.

use super::*;

impl ThumbnailCacheKey {
    pub(super) fn for_path(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        let modified_nanos = metadata
            .modified()
            .unwrap_or(UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Ok(Self {
            path: path.to_path_buf(),
            size: metadata.len(),
            modified_nanos,
        })
    }

    pub(super) fn disk_path(&self) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        thumbnail_cache_dir().join(format!("{:016x}.png", hasher.finish()))
    }
}

impl gpui::Global for ThumbnailCache {}

impl ThumbnailCache {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub(super) fn get(&mut self, key: &ThumbnailCacheKey) -> Option<ThumbnailImage> {
        let image = self.images.get(key).cloned()?;
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(key.clone());
        Some(image)
    }

    pub(super) fn insert(&mut self, key: ThumbnailCacheKey, image: ThumbnailImage) {
        self.images
            .retain(|existing, _| existing.path != key.path || existing == &key);
        self.order
            .retain(|existing| existing.path != key.path || existing == &key);
        self.images.insert(key.clone(), image);
        self.order.retain(|existing| existing != &key);
        self.order.push_back(key);
        while self.images.len() > MAX_PERSISTENT_THUMBNAILS {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.images.remove(&oldest);
        }
    }
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self::new()
    }
}

impl NewTab {
    pub(super) fn start_thumbnail_render(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.thumbnails.contains_key(&path) {
            return;
        }
        let key = match ThumbnailCacheKey::for_path(&path) {
            Ok(key) => key,
            Err(error) => {
                eprintln!(
                    "failed to fingerprint new tab preview for {}: {error:#}",
                    path.display()
                );
                self.thumbnails.insert(path, ThumbnailState::Failed);
                return;
            }
        };
        if let Some(image) = cx.global_mut::<ThumbnailCache>().get(&key) {
            self.thumbnails.insert(path, ThumbnailState::Ready(image));
            return;
        }

        self.thumbnails
            .insert(path.clone(), ThumbnailState::Loading);

        cx.spawn(async move |this, cx| {
            let render_path = path.clone();
            let render_key = key.clone();
            let result = cx
                .background_spawn(async move {
                    load_or_render_cover_thumbnail(&render_path, &render_key)
                })
                .await;

            let state = match result {
                Ok(image) => ThumbnailState::Ready(image),
                Err(error) => {
                    eprintln!(
                        "failed to render new tab preview for {}: {error:#}",
                        path.display()
                    );
                    ThumbnailState::Failed
                }
            };

            let _ = this.update(cx, |this, cx| {
                if let ThumbnailState::Ready(image) = &state {
                    cx.global_mut::<ThumbnailCache>().insert(key, image.clone());
                }
                this.thumbnails.insert(path, state);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_thumbnail(
        thumbnail: ThumbnailState,
        theme: ThemeColor,
        page_color: u32,
    ) -> AnyElement {
        match thumbnail {
            ThumbnailState::Ready(thumbnail) => {
                let image = thumbnail.image.clone();
                canvas(
                    move |_, _, _| image.clone(),
                    move |bounds, image, window, _| {
                        window.paint_quad(fill(bounds, rgb(page_color)));
                        if let Err(error) =
                            window.paint_image(bounds, bounds, Corners::default(), image, 0, false)
                        {
                            eprintln!("failed to paint new tab preview: {error:#}");
                        }
                    },
                )
                .w(px(thumbnail.width))
                .h(px(thumbnail.height))
                .border_1()
                .border_color(theme.border)
                .into_any_element()
            }
            ThumbnailState::Loading => div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(THUMBNAIL_MAX_WIDTH))
                .h(px(THUMBNAIL_MAX_HEIGHT))
                .border_1()
                .border_color(theme.border)
                .bg(rgb(page_color))
                .text_size(ui_rems(11.0))
                .text_color(theme.muted_foreground)
                .child(crate::fl!("new-tab-rendering-preview"))
                .into_any_element(),
            ThumbnailState::Failed => div()
                .w(px(THUMBNAIL_MAX_WIDTH))
                .h(px(THUMBNAIL_MAX_HEIGHT))
                .child(
                    Alert::error(
                        "new-tab-preview-unavailable",
                        crate::fl!("new-tab-preview-unavailable"),
                    )
                    .xsmall(),
                )
                .into_any_element(),
        }
    }
}

pub(super) fn thumbnail_cache_dir() -> PathBuf {
    glyphst_core::paths::state_dir()
        .join("thumbnails")
        .join(THUMBNAIL_CACHE_VERSION)
}

pub(super) fn load_or_render_cover_thumbnail(
    path: &Path,
    key: &ThumbnailCacheKey,
) -> Result<ThumbnailImage> {
    let cache_path = key.disk_path();
    if cache_path.is_file()
        && let Ok(image) = image::open(&cache_path)
    {
        let rgba = image.into_rgba8();
        let width = rgba.width() as f32;
        let height = rgba.height() as f32;
        return Ok(ThumbnailImage {
            image: Arc::new(RenderImage::new(smallvec![Frame::new(rgba)])),
            width,
            height,
        });
    }
    render_cover_thumbnail(path, Some(&cache_path))
}

pub(super) fn persist_cover_thumbnail(path: &Path, image: &RgbaImage) -> Result<()> {
    let Some(parent) = path.parent() else {
        bail!("thumbnail cache path has no parent");
    };
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create thumbnail cache {}", parent.display()))?;
    image
        .save_with_format(path, ImageFormat::Png)
        .with_context(|| format!("failed to save thumbnail cache {}", path.display()))?;

    let mut entries = fs::read_dir(parent)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata
                .is_file()
                .then(|| (metadata.modified().unwrap_or(UNIX_EPOCH), entry.path()))
        })
        .collect::<Vec<_>>();
    if entries.len() > MAX_PERSISTENT_THUMBNAILS {
        entries.sort_by_key(|(modified, _)| *modified);
        let remove_count = entries.len() - MAX_PERSISTENT_THUMBNAILS;
        for (_, path) in entries.into_iter().take(remove_count) {
            if let Err(error) = fs::remove_file(&path) {
                eprintln!(
                    "failed to prune cached new tab preview {}: {error:#}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

pub(super) fn render_cover_thumbnail(
    path: &Path,
    cache_path: Option<&Path>,
) -> Result<ThumbnailImage> {
    let mut document = mupdf::Document::open(path)?;
    if document.is_reflowable()? {
        document.layout(480.0, 640.0, 16.0)?;
    }
    let page_count = document.page_count()?;
    if page_count <= 0 {
        bail!("document has no readable content");
    }

    let page = document.load_page(0)?;
    let page_bounds = page.bounds()?;
    let page_width = page_bounds.x1 - page_bounds.x0;
    let page_height = page_bounds.y1 - page_bounds.y0;
    if page_width <= 0.0 || page_height <= 0.0 {
        bail!("PDF first page has invalid bounds");
    }

    let scale = (THUMBNAIL_MAX_WIDTH / page_width)
        .min(THUMBNAIL_MAX_HEIGHT / page_height)
        .max(0.001);
    let logical_width = page_width * scale;
    let logical_height = page_height * scale;
    let physical_width = logical_width.ceil().max(1.0) as i32;
    let physical_height = logical_height.ceil().max(1.0) as i32;

    let mut display_list = DisplayList::new(page_bounds)?;
    let device = Device::from_display_list(&mut display_list)?;
    page.run(&device, &Matrix::IDENTITY)?;
    drop(device);

    let mut pixmap = Pixmap::new_with_w_h(
        &Colorspace::device_rgb(),
        physical_width,
        physical_height,
        true,
    )?;
    pixmap.samples_mut().fill(255);

    let zoom_x = physical_width as f32 / page_width;
    let zoom_y = physical_height as f32 / page_height;
    let matrix = Matrix::new(
        zoom_x,
        0.0,
        0.0,
        zoom_y,
        -page_bounds.x0 * zoom_x,
        -page_bounds.y0 * zoom_y,
    );

    {
        let device = Device::from_pixmap(&pixmap)?;
        display_list
            .run(&device, &matrix, Rect::INF)
            .context("failed to render PDF preview")?;
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

    let rgba = RgbaImage::from_raw(width, height, bgra)
        .context("failed to create preview image from pixmap")?;
    if let Some(cache_path) = cache_path
        && let Err(error) = persist_cover_thumbnail(cache_path, &rgba)
    {
        eprintln!(
            "failed to cache new tab preview for {}: {error:#}",
            path.display()
        );
    }
    let image = Arc::new(RenderImage::new(smallvec![Frame::new(rgba)]));

    Ok(ThumbnailImage {
        image,
        width: logical_width,
        height: logical_height,
    })
}
