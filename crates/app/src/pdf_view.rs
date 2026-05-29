use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{
    Bounds, ClipboardItem, Context, Corners, FocusHandle, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, Point as GpuiPoint, Render, RenderImage, ScrollDelta, ScrollWheelEvent,
    Window, canvas, div, fill, point, prelude::*, px, rgb, rgba, size,
};
use image::{Frame, RgbaImage};
use mupdf::text_page::TextBlockType;
use mupdf::{Colorspace, Device, DisplayList, Matrix, Pixmap, Quad, Rect, TextPageFlags};
use smallvec::smallvec;

gpui::actions!(
    pdf_viewer,
    [CopySelection, SelectAll, ZoomIn, ZoomOut, ResetZoom]
);

const INITIAL_SELECTION_TOLERANCE_PX: f32 = 24.0;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 8.0;
const ZOOM_STEP: f32 = 1.25;
const SCROLL_LINE_MULTIPLIER: f32 = 20.0;

struct CachedRender {
    physical_width: u32,
    image: Arc<RenderImage>,
}

struct PaintData {
    image: Arc<RenderImage>,
    aspect_ratio: f32,
}

#[derive(Clone)]
struct Glyph {
    c: char,
    quad: Quad,
    line_idx: u32,
    block_idx: u32,
}

#[derive(Clone, Copy)]
struct Selection {
    anchor: usize,
    head: usize,
}

pub struct PdfView {
    display_list: mupdf::DisplayList,
    page_bounds: mupdf::Rect,
    page_width: f32,
    page_height: f32,
    glyphs: Vec<Glyph>,
    selection: Option<Selection>,
    dragging: bool,
    image_bounds: Option<Bounds<Pixels>>,
    focus_handle: FocusHandle,
    cached: Option<CachedRender>,
    zoom: f32,
}

impl PdfView {
    pub fn open(path: impl AsRef<std::path::Path>, focus_handle: FocusHandle) -> Result<Self> {
        let document = mupdf::Document::open(
            path.as_ref()
                .to_str()
                .ok_or_else(|| anyhow!("PDF path is not valid UTF-8"))?,
        )?;
        let page = document
            .pages()?
            .next()
            .transpose()?
            .ok_or_else(|| anyhow!("PDF has no pages"))?;
        let page_bounds = page.bounds()?;
        let page_width = page_bounds.x1 - page_bounds.x0;
        let page_height = page_bounds.y1 - page_bounds.y0;

        let display_list = mupdf::DisplayList::new(page_bounds)?;
        let device = Device::from_display_list(&display_list)?;
        page.run(&device, &Matrix::IDENTITY)?;

        let glyphs = Self::extract_glyphs(&display_list)?;

        Ok(Self {
            display_list,
            page_bounds,
            page_width,
            page_height,
            glyphs,
            selection: None,
            dragging: false,
            image_bounds: None,
            focus_handle,
            cached: None,
            zoom: 1.0,
        })
    }

    fn extract_glyphs(display_list: &DisplayList) -> Result<Vec<Glyph>> {
        let text_page = display_list
            .to_text_page(TextPageFlags::PRESERVE_WHITESPACE | TextPageFlags::ACCURATE_BBOXES)?;
        let mut glyphs = Vec::new();
        let mut line_idx = 0;

        for (block_idx, block) in text_page.blocks().enumerate() {
            if block.r#type() != TextBlockType::Text {
                continue;
            }

            for line in block.lines() {
                for ch in line.chars() {
                    if let Some(c) = ch.char() {
                        glyphs.push(Glyph {
                            c,
                            quad: ch.quad(),
                            line_idx,
                            block_idx: block_idx as u32,
                        });
                    }
                }

                line_idx += 1;
            }
        }

        Ok(glyphs)
    }

    fn paint_data(&mut self, logical_width: Pixels, scale_factor: f32) -> Result<PaintData> {
        let logical_width = logical_width.as_f32().max(1.0);
        let physical_width = (logical_width * scale_factor).ceil().max(1.0) as u32;

        if self
            .cached
            .as_ref()
            .is_none_or(|cached| cached.physical_width != physical_width)
        {
            let image = self.render_first_page(physical_width)?;
            self.cached = Some(CachedRender {
                physical_width,
                image,
            });
        }

        let image = self
            .cached
            .as_ref()
            .context("render cache was not populated")?
            .image
            .clone();

        Ok(PaintData {
            image,
            aspect_ratio: self.page_height / self.page_width,
        })
    }

    fn render_first_page(&self, physical_width: u32) -> Result<Arc<RenderImage>> {
        let zoom = physical_width as f32 / self.page_width;
        let physical_height = (self.page_height * zoom).ceil().max(1.0) as i32;
        let physical_width = physical_width as i32;

        let mut pixmap = Pixmap::new_with_w_h(
            &Colorspace::device_rgb(),
            physical_width,
            physical_height,
            true,
        )?;
        pixmap.samples_mut().fill(255);

        let matrix = Matrix::new(
            zoom,
            0.0,
            0.0,
            zoom,
            -self.page_bounds.x0 * zoom,
            -self.page_bounds.y0 * zoom,
        );
        {
            let device = Device::from_pixmap(&pixmap)?;
            self.display_list
                .run(&device, &matrix, Rect::INF)
                .context("failed to render first PDF page")?;
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
        let Some(bounds) = self.image_bounds else {
            return false;
        };

        let x = point.x.as_f32();
        let y = point.y.as_f32();
        let x0 = bounds.origin.x.as_f32();
        let y0 = bounds.origin.y.as_f32();
        let x1 = x0 + bounds.size.width.as_f32();
        let y1 = y0 + bounds.size.height.as_f32();

        x >= x0 && x <= x1 && y >= y0 && y <= y1
    }

    fn screen_to_pdf(&self, point: GpuiPoint<Pixels>) -> Option<mupdf::Point> {
        let bounds = self.image_bounds?;
        let scale = bounds.size.width.as_f32() / self.page_width;
        if scale <= 0.0 {
            return None;
        }

        Some(mupdf::Point {
            x: (point.x.as_f32() - bounds.origin.x.as_f32()) / scale + self.page_bounds.x0,
            y: (point.y.as_f32() - bounds.origin.y.as_f32()) / scale + self.page_bounds.y0,
        })
    }

    fn glyph_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        self.screen_to_pdf(point)
            .and_then(|point| self.glyph_at_pdf(point))
    }

    fn nearby_glyph_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        let bounds = self.image_bounds?;
        let scale = bounds.size.width.as_f32() / self.page_width;
        if scale <= 0.0 {
            return None;
        }

        let point = self.screen_to_pdf(point)?;
        self.nearby_glyph_at_pdf(point, INITIAL_SELECTION_TOLERANCE_PX / scale)
    }

    fn exact_glyph_at_pdf(&self, point: mupdf::Point) -> Option<usize> {
        self.glyphs
            .iter()
            .position(|glyph| Self::quad_rect(&glyph.quad).contains(point.x, point.y))
    }

    fn nearby_glyph_at_pdf(&self, point: mupdf::Point, tolerance: f32) -> Option<usize> {
        if let Some(ix) = self.exact_glyph_at_pdf(point) {
            return Some(ix);
        }

        self.glyphs
            .iter()
            .enumerate()
            .filter_map(|(ix, glyph)| {
                let distance = Self::distance_to_rect(point, Self::quad_rect(&glyph.quad));
                (distance <= tolerance).then_some((ix, distance))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(ix, _)| ix)
    }

    fn glyph_at_pdf(&self, point: mupdf::Point) -> Option<usize> {
        if self.glyphs.is_empty() {
            return None;
        }

        if let Some(ix) = self.exact_glyph_at_pdf(point) {
            return Some(ix);
        }

        let (_, start, end, line_rect) = self.nearest_line(point)?;
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

    fn nearest_line(&self, point: mupdf::Point) -> Option<(f32, usize, usize, Rect)> {
        let mut best = None;
        let mut start = 0;

        while start < self.glyphs.len() {
            let line_idx = self.glyphs[start].line_idx;
            let mut end = start;
            let mut rect = Self::quad_rect(&self.glyphs[start].quad);

            while end + 1 < self.glyphs.len() && self.glyphs[end + 1].line_idx == line_idx {
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
        if (zoom - self.zoom).abs() > f32::EPSILON {
            self.zoom = zoom;
            cx.notify();
        }
    }

    fn zoom_in(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(self.zoom * ZOOM_STEP, cx);
    }

    fn zoom_out(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(self.zoom / ZOOM_STEP, cx);
    }

    fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(1.0, cx);
    }

    fn handle_scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if !event.modifiers.control {
            return;
        }

        let delta = match event.delta {
            ScrollDelta::Pixels(pixels) => pixels.y.as_f32(),
            ScrollDelta::Lines(lines) => lines.y * SCROLL_LINE_MULTIPLIER,
        };

        if delta == 0.0 {
            return;
        }

        let factor = if delta > 0.0 {
            1.0 + delta.abs() * 0.01
        } else {
            1.0 / (1.0 + delta.abs() * 0.01)
        };

        self.set_zoom(self.zoom * factor, cx);
    }

    fn selection_highlights(&self, image_bounds: Bounds<Pixels>) -> Vec<Bounds<Pixels>> {
        let Some((start, end)) = self.selection_bounds() else {
            return Vec::new();
        };

        let mut highlights = Vec::new();
        let mut current_line = None;
        let mut current_rect = None;

        for glyph in &self.glyphs[start..=end] {
            let rect = Self::quad_rect(&glyph.quad);

            match (current_line, current_rect) {
                (Some(line), Some(existing)) if line == glyph.line_idx => {
                    current_rect = Some(Self::union_rect(existing, rect));
                }
                (Some(_), Some(existing)) => {
                    highlights.push(self.pdf_rect_to_screen(existing, image_bounds));
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
                _ => {
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
            }
        }

        if let Some(rect) = current_rect {
            highlights.push(self.pdf_rect_to_screen(rect, image_bounds));
        }

        highlights
    }

    fn pdf_rect_to_screen(&self, rect: Rect, image_bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        let scale = image_bounds.size.width.as_f32() / self.page_width;
        let x0 = image_bounds.origin.x.as_f32() + (rect.x0 - self.page_bounds.x0) * scale;
        let y0 = image_bounds.origin.y.as_f32() + (rect.y0 - self.page_bounds.y0) * scale;
        let x1 = image_bounds.origin.x.as_f32() + (rect.x1 - self.page_bounds.x0) * scale;
        let y1 = image_bounds.origin.y.as_f32() + (rect.y1 - self.page_bounds.y0) * scale;

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
        let paint_view = view.clone();

        div()
            .size_full()
            .bg(rgb(0x202020))
            .cursor_text()
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
                canvas(
                    move |bounds, window, cx| {
                        prepaint_view.update(cx, |this, _| {
                            this.paint_data(bounds.size.width * this.zoom, window.scale_factor())
                                .expect("failed to render PDF")
                        })
                    },
                    move |bounds, paint_data, window, cx| {
                        let image_width = paint_view.read(cx).zoom * bounds.size.width;
                        let image_height = image_width * paint_data.aspect_ratio;
                        let origin = point(
                            bounds.origin.x + (bounds.size.width - image_width) / 2.0,
                            bounds.origin.y,
                        );
                        let image_bounds = Bounds::new(origin, size(image_width, image_height));

                        let highlights = paint_view.update(cx, |this, _| {
                            this.image_bounds = Some(image_bounds);
                            this.selection_highlights(image_bounds)
                        });

                        let _ = window.paint_image(
                            image_bounds,
                            Corners::default(),
                            paint_data.image,
                            0,
                            false,
                        );

                        for highlight in highlights {
                            window.paint_quad(fill(highlight, rgba(0x61afef66)));
                        }
                    },
                )
                .size_full(),
            )
    }
}
