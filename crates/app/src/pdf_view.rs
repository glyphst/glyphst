use std::{ops::Range, sync::Arc};

use crate::command_palette::{CommandPalette, PaletteEvent};
use crate::settings::{Settings, SettingsStore};
use crate::settings_panel::{PanelEvent, SettingsPanel};
use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{
    Bounds, ClipboardItem, Context, Corners, Entity, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point as GpuiPoint, Render, RenderImage,
    ScrollDelta, ScrollWheelEvent, SharedString, Task, Window, canvas, div, fill, point,
    prelude::*, px, rgb, rgba, size,
};
use image::{Frame, RgbaImage};
use mupdf::text_page::TextBlockType;
use mupdf::{Colorspace, Device, DisplayList, Matrix, Pixmap, Quad, Rect, TextPageFlags};
use smallvec::smallvec;

gpui::actions!(
    pdf_viewer,
    [
        CopySelection,
        SelectAll,
        ZoomIn,
        ZoomOut,
        ResetZoom,
        ToggleCommandPalette,
        OpenSettings
    ]
);

const INITIAL_SELECTION_TOLERANCE_PX: f32 = 24.0;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 8.0;
const SCROLL_LINE_MULTIPLIER: f32 = 20.0;
const MAX_CACHED_RENDERS: usize = 12;

struct PdfPage {
    display_list: Arc<DisplayList>,
    bounds: Rect,
    width: f32,
    height: f32,
    offset_y: f32,
    glyph_range: Range<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct RenderKey {
    page_idx: usize,
    physical_width: u32,
    physical_height: u32,
}

struct CachedRender {
    key: RenderKey,
    render_scale: f32,
    last_access: u64,
    image: Arc<RenderImage>,
}

struct PendingRender {
    key: RenderKey,
    _task: Task<()>,
}

struct PaintPage {
    bounds: Bounds<Pixels>,
    image: Option<Arc<RenderImage>>,
}

struct PaintData {
    pages: Vec<PaintPage>,
    highlights: Vec<Bounds<Pixels>>,
}

#[derive(Clone)]
struct Glyph {
    c: char,
    quad: Quad,
    page_idx: usize,
    line_idx: u32,
    block_idx: u32,
}

#[derive(Clone, Copy)]
struct Selection {
    anchor: usize,
    head: usize,
}

pub struct PdfView {
    file_name: SharedString,
    pages: Vec<PdfPage>,
    max_page_width: f32,
    content_height_doc: f32,
    glyphs: Vec<Glyph>,
    selection: Option<Selection>,
    dragging: bool,
    page_image_bounds: Vec<Option<Bounds<Pixels>>>,
    focus_handle: FocusHandle,
    cached: Vec<CachedRender>,
    cache_access_counter: u64,
    pending_renders: Vec<PendingRender>,
    zoom: f32,
    base_scale: f32,
    viewport_height: f32,
    scroll_y_doc: f32,
    command_palette: Option<Entity<CommandPalette>>,
    settings_panel: Option<Entity<SettingsPanel>>,
}

impl PdfView {
    pub fn open(
        path: impl AsRef<std::path::Path>,
        focus_handle: FocusHandle,
        settings: Settings,
    ) -> Result<Self> {
        let file_name = path
            .as_ref()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Untitled")
            .to_string()
            .into();

        let document = mupdf::Document::open(
            path.as_ref()
                .to_str()
                .ok_or_else(|| anyhow!("PDF path is not valid UTF-8"))?,
        )?;
        let page_count = document.page_count()?;
        if page_count == 0 {
            bail!("PDF has no pages");
        }

        let mut pages = Vec::with_capacity(page_count as usize);
        let mut glyphs = Vec::new();
        let mut offset_y = 0.0;
        let mut max_page_width = 0.0_f32;
        let mut next_line_idx = 0;
        let mut next_block_idx = 0;

        for page_idx in 0..page_count {
            let page = document.load_page(page_idx)?;
            let page_bounds = page.bounds()?;
            let page_width = page_bounds.x1 - page_bounds.x0;
            let page_height = page_bounds.y1 - page_bounds.y0;

            let display_list = Arc::new(DisplayList::new(page_bounds)?);
            let device = Device::from_display_list(&display_list)?;
            page.run(&device, &Matrix::IDENTITY)?;

            let glyph_range = Self::extract_glyphs(
                &display_list,
                page_idx as usize,
                &mut glyphs,
                &mut next_line_idx,
                &mut next_block_idx,
            )?;

            pages.push(PdfPage {
                display_list,
                bounds: page_bounds,
                width: page_width,
                height: page_height,
                offset_y,
                glyph_range,
            });

            max_page_width = max_page_width.max(page_width);
            offset_y += page_height + settings.page_gap;
        }

        let content_height_doc = offset_y - settings.page_gap;
        let page_image_bounds = (0..pages.len()).map(|_| None).collect();

        Ok(Self {
            file_name,
            pages,
            max_page_width,
            content_height_doc,
            glyphs,
            selection: None,
            dragging: false,
            page_image_bounds,
            focus_handle,
            cached: Vec::new(),
            cache_access_counter: 0,
            pending_renders: Vec::new(),
            zoom: settings.default_zoom,
            base_scale: 1.0,
            viewport_height: 0.0,
            scroll_y_doc: 0.0,
            command_palette: None,
            settings_panel: None,
        })
    }

    fn extract_glyphs(
        display_list: &DisplayList,
        page_idx: usize,
        glyphs: &mut Vec<Glyph>,
        next_line_idx: &mut u32,
        next_block_idx: &mut u32,
    ) -> Result<Range<usize>> {
        let start = glyphs.len();
        let text_page = display_list
            .to_text_page(TextPageFlags::PRESERVE_WHITESPACE | TextPageFlags::ACCURATE_BBOXES)?;

        for block in text_page.blocks() {
            if block.r#type() != TextBlockType::Text {
                continue;
            }

            let block_idx = *next_block_idx;
            *next_block_idx = next_block_idx.saturating_add(1);

            for line in block.lines() {
                let line_idx = *next_line_idx;
                for ch in line.chars() {
                    if let Some(c) = ch.char() {
                        glyphs.push(Glyph {
                            c,
                            quad: ch.quad(),
                            page_idx,
                            line_idx,
                            block_idx,
                        });
                    }
                }

                *next_line_idx = next_line_idx.saturating_add(1);
            }
        }

        Ok(start..glyphs.len())
    }

    fn paint_data(
        &mut self,
        bounds: Bounds<Pixels>,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> Result<PaintData> {
        let viewport_width = bounds.size.width.as_f32().max(1.0);
        self.viewport_height = bounds.size.height.as_f32().max(1.0);
        self.base_scale = viewport_width / self.max_page_width.max(1.0);
        self.clamp_scroll();

        self.page_image_bounds.clear();
        self.page_image_bounds
            .resize_with(self.pages.len(), || None);

        let scale = self.effective_scale();
        let mut visible_pages = Vec::new();
        let viewport_top = bounds.origin.y.as_f32();
        let viewport_bottom = viewport_top + self.viewport_height;

        for page_idx in 0..self.pages.len() {
            let (image_bounds, _) =
                self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
            let page_top = image_bounds.origin.y.as_f32();
            let page_bottom = page_top + image_bounds.size.height.as_f32();

            if page_bottom >= viewport_top && page_top <= viewport_bottom {
                self.page_image_bounds[page_idx] = Some(image_bounds);
                visible_pages.push(page_idx);
            }
        }

        if let (Some(first), Some(last)) = (visible_pages.first(), visible_pages.last()) {
            let start = first.saturating_sub(1);
            let end = (*last + 1).min(self.pages.len().saturating_sub(1));
            for page_idx in start..=end {
                if page_idx >= *first && page_idx <= *last {
                    continue;
                }

                let (_, key) =
                    self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
                self.request_render(key, cx);
            }
        }

        let mut pages = Vec::with_capacity(visible_pages.len());
        for page_idx in visible_pages {
            let (image_bounds, key) =
                self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
            let image = self.cached_or_request_render(key, cx);
            pages.push(PaintPage {
                bounds: image_bounds,
                image,
            });
        }

        let highlights = self.selection_highlights();

        Ok(PaintData { pages, highlights })
    }

    fn image_bounds_for_page(
        &self,
        page_idx: usize,
        bounds: Bounds<Pixels>,
        scale: f32,
    ) -> Bounds<Pixels> {
        let page = &self.pages[page_idx];
        let image_width = px(page.width * scale);
        let image_height = px(page.height * scale);
        let origin = point(
            bounds.origin.x + (bounds.size.width - image_width) / 2.0,
            bounds.origin.y + px((page.offset_y - self.scroll_y_doc) * scale),
        );

        Bounds::new(origin, size(image_width, image_height))
    }

    fn image_bounds_and_render_key(
        &self,
        page_idx: usize,
        bounds: Bounds<Pixels>,
        scale: f32,
        scale_factor: f32,
    ) -> (Bounds<Pixels>, RenderKey) {
        let image_bounds = self.image_bounds_for_page(page_idx, bounds, scale);
        let key = Self::render_key(page_idx, image_bounds, scale_factor);
        (
            Self::pixel_aligned_image_bounds(image_bounds, key, scale_factor),
            key,
        )
    }

    fn render_key(page_idx: usize, image_bounds: Bounds<Pixels>, scale_factor: f32) -> RenderKey {
        let scale_factor = scale_factor.max(0.001);
        RenderKey {
            page_idx,
            physical_width: (image_bounds.size.width.as_f32() * scale_factor)
                .ceil()
                .max(1.0) as u32,
            physical_height: (image_bounds.size.height.as_f32() * scale_factor)
                .ceil()
                .max(1.0) as u32,
        }
    }

    fn pixel_aligned_image_bounds(
        image_bounds: Bounds<Pixels>,
        key: RenderKey,
        scale_factor: f32,
    ) -> Bounds<Pixels> {
        let scale_factor = scale_factor.max(0.001);
        Bounds::new(
            image_bounds.origin,
            size(
                px(key.physical_width as f32 / scale_factor),
                px(key.physical_height as f32 / scale_factor),
            ),
        )
    }

    fn effective_scale(&self) -> f32 {
        (self.base_scale * self.zoom).max(0.001)
    }

    fn max_scroll_y_doc(&self) -> f32 {
        let viewport_height_doc = self.viewport_height / self.effective_scale();
        (self.content_height_doc - viewport_height_doc).max(0.0)
    }

    fn recompute_layout(&mut self, page_gap: f32) {
        let mut offset_y = 0.0;
        for page in &mut self.pages {
            page.offset_y = offset_y;
            offset_y += page.height + page_gap;
        }

        self.content_height_doc = offset_y - page_gap;
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        self.scroll_y_doc = self.scroll_y_doc.clamp(0.0, self.max_scroll_y_doc());
    }

    fn set_scroll_y_doc(&mut self, scroll_y_doc: f32, cx: &mut Context<Self>) {
        let previous = self.scroll_y_doc;
        self.scroll_y_doc = scroll_y_doc;
        self.clamp_scroll();

        if (self.scroll_y_doc - previous).abs() > f32::EPSILON {
            cx.notify();
        }
    }

    fn request_render(&mut self, key: RenderKey, cx: &mut Context<Self>) {
        if self.exact_cached_render_index(key).is_none() {
            self.start_background_render(key, cx);
        }
    }

    fn cached_or_request_render(
        &mut self,
        key: RenderKey,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        if let Some(index) = self.exact_cached_render_index(key) {
            return Some(self.touch_cached_render(index));
        }

        self.start_background_render(key, cx);
        self.scaled_fallback_render_index(key)
            .map(|index| self.touch_cached_render(index))
    }

    fn exact_cached_render_index(&self, key: RenderKey) -> Option<usize> {
        self.cached.iter().position(|cached| cached.key == key)
    }

    fn scaled_fallback_render_index(&self, key: RenderKey) -> Option<usize> {
        let render_scale = key.physical_width as f32 / self.pages[key.page_idx].width;
        self.cached
            .iter()
            .enumerate()
            .filter(|(_, cached)| {
                cached.key.page_idx == key.page_idx
                    && cached.key.physical_width <= key.physical_width
                    && cached.key.physical_height <= key.physical_height
            })
            .min_by(|(_, a), (_, b)| {
                (a.render_scale - render_scale)
                    .abs()
                    .total_cmp(&(b.render_scale - render_scale).abs())
            })
            .map(|(index, _)| index)
    }

    fn touch_cached_render(&mut self, index: usize) -> Arc<RenderImage> {
        let last_access = self.next_cache_access();
        self.cached[index].last_access = last_access;
        self.cached[index].image.clone()
    }

    fn insert_cached_render(&mut self, key: RenderKey, image: Arc<RenderImage>) {
        let last_access = self.next_cache_access();
        let render_scale = key.physical_width as f32 / self.pages[key.page_idx].width;

        if let Some(index) = self.exact_cached_render_index(key) {
            self.cached[index] = CachedRender {
                key,
                render_scale,
                last_access,
                image,
            };
        } else {
            self.cached.push(CachedRender {
                key,
                render_scale,
                last_access,
                image,
            });
        }

        while self.cached.len() > MAX_CACHED_RENDERS {
            if let Some(index) = self
                .cached
                .iter()
                .enumerate()
                .min_by_key(|(_, cached)| cached.last_access)
                .map(|(index, _)| index)
            {
                self.cached.remove(index);
            }
        }
    }

    fn next_cache_access(&mut self) -> u64 {
        self.cache_access_counter = self.cache_access_counter.wrapping_add(1);
        self.cache_access_counter
    }

    fn start_background_render(&mut self, key: RenderKey, cx: &mut Context<Self>) {
        if self
            .pending_renders
            .iter()
            .any(|pending| pending.key == key)
        {
            return;
        }
        self.pending_renders
            .retain(|pending| pending.key.page_idx != key.page_idx);

        let page = &self.pages[key.page_idx];
        let display_list = page.display_list.clone();
        let page_bounds = page.bounds;
        let page_width = page.width;
        let page_height = page.height;

        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    Self::render_page(
                        &display_list,
                        page_bounds,
                        page_width,
                        page_height,
                        key.physical_width,
                        key.physical_height,
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

                    match result {
                        Ok(image) => {
                            this.insert_cached_render(key, image);
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

    fn render_page(
        display_list: &DisplayList,
        page_bounds: Rect,
        page_width: f32,
        page_height: f32,
        physical_width: u32,
        physical_height: u32,
    ) -> Result<Arc<RenderImage>> {
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

    fn point_in_image(&self, point: GpuiPoint<Pixels>) -> bool {
        self.page_at_screen(point).is_some()
    }

    fn page_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        self.page_image_bounds
            .iter()
            .enumerate()
            .find_map(|(page_idx, bounds)| {
                let bounds = bounds.as_ref()?;
                let x = point.x.as_f32();
                let y = point.y.as_f32();
                let x0 = bounds.origin.x.as_f32();
                let y0 = bounds.origin.y.as_f32();
                let x1 = x0 + bounds.size.width.as_f32();
                let y1 = y0 + bounds.size.height.as_f32();

                (x >= x0 && x <= x1 && y >= y0 && y <= y1).then_some(page_idx)
            })
    }

    fn screen_to_pdf(&self, page_idx: usize, point: GpuiPoint<Pixels>) -> Option<mupdf::Point> {
        let bounds = self.page_image_bounds.get(page_idx)?.as_ref()?;
        let page = &self.pages[page_idx];
        let scale_x = bounds.size.width.as_f32() / page.width;
        let scale_y = bounds.size.height.as_f32() / page.height;
        if scale_x <= 0.0 || scale_y <= 0.0 {
            return None;
        }

        Some(mupdf::Point {
            x: (point.x.as_f32() - bounds.origin.x.as_f32()) / scale_x + page.bounds.x0,
            y: (point.y.as_f32() - bounds.origin.y.as_f32()) / scale_y + page.bounds.y0,
        })
    }

    fn glyph_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        let page_idx = self.page_at_screen(point)?;
        self.screen_to_pdf(page_idx, point)
            .and_then(|point| self.glyph_at_pdf(page_idx, point))
    }

    fn nearby_glyph_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        let page_idx = self.page_at_screen(point)?;
        let bounds = self.page_image_bounds.get(page_idx)?.as_ref()?;
        let page = &self.pages[page_idx];
        let scale = (bounds.size.width.as_f32() / page.width)
            .min(bounds.size.height.as_f32() / page.height);
        if scale <= 0.0 {
            return None;
        }

        let point = self.screen_to_pdf(page_idx, point)?;
        self.nearby_glyph_at_pdf(page_idx, point, INITIAL_SELECTION_TOLERANCE_PX / scale)
    }

    fn exact_glyph_at_pdf(&self, page_idx: usize, point: mupdf::Point) -> Option<usize> {
        let range = self.pages[page_idx].glyph_range.clone();
        self.glyphs[range.clone()]
            .iter()
            .enumerate()
            .find_map(|(offset, glyph)| {
                Self::quad_rect(&glyph.quad)
                    .contains(point.x, point.y)
                    .then_some(range.start + offset)
            })
    }

    fn nearby_glyph_at_pdf(
        &self,
        page_idx: usize,
        point: mupdf::Point,
        tolerance: f32,
    ) -> Option<usize> {
        if let Some(ix) = self.exact_glyph_at_pdf(page_idx, point) {
            return Some(ix);
        }

        let range = self.pages[page_idx].glyph_range.clone();
        self.glyphs[range.clone()]
            .iter()
            .enumerate()
            .filter_map(|(offset, glyph)| {
                let distance = Self::distance_to_rect(point, Self::quad_rect(&glyph.quad));
                (distance <= tolerance).then_some((range.start + offset, distance))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(ix, _)| ix)
    }

    fn glyph_at_pdf(&self, page_idx: usize, point: mupdf::Point) -> Option<usize> {
        if self.pages[page_idx].glyph_range.is_empty() {
            return None;
        }

        if let Some(ix) = self.exact_glyph_at_pdf(page_idx, point) {
            return Some(ix);
        }

        let (_, start, end, line_rect) = self.nearest_line(page_idx, point)?;
        if point.x <= line_rect.x0 {
            return Some(start);
        }
        if point.x >= line_rect.x1 {
            return Some(end);
        }

        let mut nearest = start;
        let mut nearest_dist = f32::INFINITY;
        for ix in start..=end {
            let rect = Self::quad_rect(&self.glyphs[ix].quad);
            let center_x = (rect.x0 + rect.x1) / 2.0;
            let dist = (point.x - center_x).abs();
            if dist < nearest_dist {
                nearest = ix;
                nearest_dist = dist;
            }
        }

        Some(nearest)
    }

    fn nearest_line(
        &self,
        page_idx: usize,
        point: mupdf::Point,
    ) -> Option<(f32, usize, usize, Rect)> {
        let range = self.pages[page_idx].glyph_range.clone();
        let mut best = None;
        let mut start = range.start;

        while start < range.end {
            let line_idx = self.glyphs[start].line_idx;
            let mut end = start;
            let mut rect = Self::quad_rect(&self.glyphs[start].quad);

            while end + 1 < range.end && self.glyphs[end + 1].line_idx == line_idx {
                end += 1;
                rect = Self::union_rect(rect, Self::quad_rect(&self.glyphs[end].quad));
            }

            let dy = if point.y < rect.y0 {
                rect.y0 - point.y
            } else if point.y > rect.y1 {
                point.y - rect.y1
            } else {
                0.0
            };

            if best.as_ref().is_none_or(|(best_dy, _, _, _)| dy < *best_dy) {
                best = Some((dy, start, end, rect));
            }

            start = end + 1;
        }

        best
    }

    fn selection_bounds(&self) -> Option<(usize, usize)> {
        let selection = self.selection?;
        let start = selection.anchor.min(selection.head);
        let end = selection.anchor.max(selection.head);

        if start < self.glyphs.len() && end < self.glyphs.len() {
            Some((start, end))
        } else {
            None
        }
    }

    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_bounds()?;
        let mut text = String::new();
        let mut previous: Option<&Glyph> = None;

        for glyph in &self.glyphs[start..=end] {
            if let Some(previous) = previous {
                if glyph.block_idx != previous.block_idx {
                    text.push_str("\n\n");
                } else if glyph.line_idx != previous.line_idx {
                    text.push('\n');
                }
            }

            text.push(glyph.c);
            previous = Some(glyph);
        }

        (!text.is_empty()).then_some(text)
    }

    fn select_all(&mut self) {
        if self.glyphs.is_empty() {
            self.selection = None;
            return;
        }

        self.selection = Some(Selection {
            anchor: 0,
            head: self.glyphs.len() - 1,
        });
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.zoom).abs() <= f32::EPSILON {
            return;
        }

        let old_scale = self.effective_scale();
        let center_doc = self.scroll_y_doc + self.viewport_height / (2.0 * old_scale);

        self.zoom = zoom;

        let new_scale = self.effective_scale();
        self.scroll_y_doc = center_doc - self.viewport_height / (2.0 * new_scale);
        self.clamp_scroll();
        cx.notify();
    }

    fn zoom_in(&mut self, cx: &mut Context<Self>) {
        let zoom_step = cx.global::<SettingsStore>().settings.zoom_step;
        self.set_zoom(self.zoom * zoom_step, cx);
    }

    fn zoom_out(&mut self, cx: &mut Context<Self>) {
        let zoom_step = cx.global::<SettingsStore>().settings.zoom_step;
        self.set_zoom(self.zoom / zoom_step, cx);
    }

    fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        let default_zoom = cx.global::<SettingsStore>().settings.default_zoom;
        self.set_zoom(default_zoom, cx);
    }

    fn current_page(&self) -> usize {
        let scale = self.effective_scale();
        let center_doc = self.scroll_y_doc + self.viewport_height / (2.0 * scale);
        let mut current = 0;

        for (idx, page) in self.pages.iter().enumerate() {
            if center_doc >= page.offset_y {
                current = idx;
            } else {
                break;
            }
        }

        current + 1
    }

    fn handle_scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = match event.delta {
            ScrollDelta::Pixels(pixels) => pixels.y.as_f32(),
            ScrollDelta::Lines(lines) => lines.y * SCROLL_LINE_MULTIPLIER,
        };

        if delta == 0.0 {
            return;
        }

        if event.modifiers.control {
            let factor = if delta > 0.0 {
                1.0 + delta.abs() * 0.01
            } else {
                1.0 / (1.0 + delta.abs() * 0.01)
            };

            self.set_zoom(self.zoom * factor, cx);
        } else {
            self.set_scroll_y_doc(self.scroll_y_doc - delta / self.effective_scale(), cx);
        }
    }

    fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_some() {
            self.close_command_palette(window, cx);
            return;
        }
        self.close_settings_panel(window, cx);

        let actions = window
            .available_actions(cx)
            .into_iter()
            .filter(|action| !action.partial_eq(&ToggleCommandPalette))
            .collect::<Vec<_>>();
        let previous_focus_handle = self.focus_handle.clone();
        let palette = cx.new(|cx| CommandPalette::new(previous_focus_handle.clone(), actions, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PaletteEvent, window, cx| {
            this.close_command_palette(window, cx);
        })
        .detach();

        palette.focus_handle(cx).focus(window, cx);
        self.command_palette = Some(palette);
        cx.notify();
    }

    fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn toggle_settings_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_panel.is_some() {
            self.close_settings_panel(window, cx);
            return;
        }
        self.close_command_palette(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let panel = cx.new(|cx| SettingsPanel::new(previous_focus_handle.clone(), cx));

        cx.subscribe_in(
            &panel,
            window,
            |this, _, event: &PanelEvent, window, cx| match event {
                PanelEvent::Dismissed => this.close_settings_panel(window, cx),
                PanelEvent::Changed => {
                    let page_gap = cx.global::<SettingsStore>().settings.page_gap;
                    this.recompute_layout(page_gap);
                    cx.notify();
                }
            },
        )
        .detach();

        panel.focus_handle(cx).focus(window, cx);
        self.settings_panel = Some(panel);
        cx.notify();
    }

    fn close_settings_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_panel.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn selection_highlights(&self) -> Vec<Bounds<Pixels>> {
        let Some((start, end)) = self.selection_bounds() else {
            return Vec::new();
        };

        let mut highlights = Vec::new();
        let mut current_page = None;
        let mut current_line = None;
        let mut current_rect = None;

        for glyph in &self.glyphs[start..=end] {
            let rect = Self::quad_rect(&glyph.quad);

            match (current_page, current_line, current_rect) {
                (Some(page), Some(line), Some(existing))
                    if page == glyph.page_idx && line == glyph.line_idx =>
                {
                    current_rect = Some(Self::union_rect(existing, rect));
                }
                (Some(page), Some(_), Some(existing)) => {
                    self.push_highlight(&mut highlights, page, existing);
                    current_page = Some(glyph.page_idx);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
                _ => {
                    current_page = Some(glyph.page_idx);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
            }
        }

        if let (Some(page), Some(rect)) = (current_page, current_rect) {
            self.push_highlight(&mut highlights, page, rect);
        }

        highlights
    }

    fn push_highlight(&self, highlights: &mut Vec<Bounds<Pixels>>, page_idx: usize, rect: Rect) {
        let Some(Some(image_bounds)) = self.page_image_bounds.get(page_idx) else {
            return;
        };

        highlights.push(self.pdf_rect_to_screen(page_idx, rect, *image_bounds));
    }

    fn pdf_rect_to_screen(
        &self,
        page_idx: usize,
        rect: Rect,
        image_bounds: Bounds<Pixels>,
    ) -> Bounds<Pixels> {
        let page = &self.pages[page_idx];
        let scale_x = image_bounds.size.width.as_f32() / page.width;
        let scale_y = image_bounds.size.height.as_f32() / page.height;
        let x0 = image_bounds.origin.x.as_f32() + (rect.x0 - page.bounds.x0) * scale_x;
        let y0 = image_bounds.origin.y.as_f32() + (rect.y0 - page.bounds.y0) * scale_y;
        let x1 = image_bounds.origin.x.as_f32() + (rect.x1 - page.bounds.x0) * scale_x;
        let y1 = image_bounds.origin.y.as_f32() + (rect.y1 - page.bounds.y0) * scale_y;

        Bounds::new(
            point(px(x0), px(y0)),
            size(px((x1 - x0).max(1.0)), px((y1 - y0).max(1.0))),
        )
    }

    fn quad_rect(quad: &Quad) -> Rect {
        Rect::new(
            quad.ul.x.min(quad.ur.x).min(quad.ll.x).min(quad.lr.x),
            quad.ul.y.min(quad.ur.y).min(quad.ll.y).min(quad.lr.y),
            quad.ul.x.max(quad.ur.x).max(quad.ll.x).max(quad.lr.x),
            quad.ul.y.max(quad.ur.y).max(quad.ll.y).max(quad.lr.y),
        )
    }

    fn union_rect(a: Rect, b: Rect) -> Rect {
        Rect::new(
            a.x0.min(b.x0),
            a.y0.min(b.y0),
            a.x1.max(b.x1),
            a.y1.max(b.y1),
        )
    }

    fn distance_to_rect(point: mupdf::Point, rect: Rect) -> f32 {
        let dx = if point.x < rect.x0 {
            rect.x0 - point.x
        } else if point.x > rect.x1 {
            point.x - rect.x1
        } else {
            0.0
        };
        let dy = if point.y < rect.y0 {
            rect.y0 - point.y
        } else if point.y > rect.y1 {
            point.y - rect.y1
        } else {
            0.0
        };

        dx.hypot(dy)
    }
}

impl Render for PdfView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let prepaint_view = view.clone();
        let settings = cx.global::<SettingsStore>().settings;
        let selection_highlight = settings.selection_highlight;

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(settings.background))
            .cursor_text()
            .key_context("PdfView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| {
                if let Some(text) = this.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.select_all();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                this.zoom_in(cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                this.zoom_out(cx);
            }))
            .on_action(cx.listener(|this, _: &ResetZoom, _, cx| {
                this.reset_zoom(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.toggle_settings_panel(window, cx);
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                this.handle_scroll_wheel(event, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.focus_handle.focus(window, cx);
                    this.dragging = false;

                    if !this.point_in_image(event.position) {
                        this.selection = None;
                        cx.notify();
                        return;
                    }

                    if let Some(ix) = this.nearby_glyph_at_screen(event.position) {
                        this.selection = Some(Selection {
                            anchor: ix,
                            head: ix,
                        });
                        this.dragging = true;
                    } else {
                        this.selection = None;
                    }

                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if !this.dragging {
                    return;
                }

                if let Some(ix) = this.glyph_at_screen(event.position) {
                    if let Some(selection) = this.selection.as_mut() {
                        selection.head = ix;
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.dragging = false;
                }),
            )
            .child(
                div().flex_1().min_h_0().child(
                    canvas(
                        move |bounds, window, cx| {
                            prepaint_view.update(cx, |this, cx| {
                                this.paint_data(bounds, window.scale_factor(), cx)
                                    .expect("failed to render PDF")
                            })
                        },
                        move |_, paint_data, window, _| {
                            for page in paint_data.pages {
                                window.paint_quad(fill(page.bounds, rgba(0xffffffff)));

                                if let Some(image) = page.image {
                                    let _ = window.paint_image(
                                        page.bounds,
                                        Corners::default(),
                                        image,
                                        0,
                                        false,
                                    );
                                }
                            }

                            for highlight in paint_data.highlights {
                                window.paint_quad(fill(highlight, rgba(selection_highlight)));
                            }
                        },
                    )
                    .size_full(),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .flex_none()
                    .h(px(26.0))
                    .px_3()
                    .border_t(px(1.0))
                    .border_color(rgb(0x303030))
                    .bg(rgb(0x181818))
                    .text_size(px(12.0))
                    .text_color(rgb(0xaaaaaa))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(self.file_name.clone()),
                    )
                    .child(div().flex_none().child(format!(
                        "{} / {}",
                        self.current_page(),
                        self.pages.len()
                    ))),
            )
            .when_some(self.command_palette.clone(), |this, palette| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_start()
                        .justify_center()
                        .pt(px(96.0))
                        .bg(rgba(0x00000066))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                this.close_command_palette(window, cx);
                            }),
                        )
                        .child(palette),
                )
            })
            .when_some(self.settings_panel.clone(), |this, panel| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_start()
                        .justify_center()
                        .pt(px(96.0))
                        .bg(rgba(0x00000066))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                this.close_settings_panel(window, cx);
                            }),
                        )
                        .child(panel),
                )
            })
    }
}
