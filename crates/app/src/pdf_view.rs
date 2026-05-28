use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{
    Bounds, Context, Corners, IntoElement, Pixels, Render, RenderImage, Window, canvas, div,
    prelude::*, rgb, size,
};
use image::{Frame, RgbaImage};
use mupdf::{Colorspace, Device, Matrix, Pixmap, Rect};
use smallvec::smallvec;

struct CachedRender {
    physical_width: u32,
    image: Arc<RenderImage>,
}

struct PaintData {
    image: Arc<RenderImage>,
    aspect_ratio: f32,
}

pub struct PdfView {
    display_list: mupdf::DisplayList,
    page_bounds: mupdf::Rect,
    page_width: f32,
    page_height: f32,
    cached: Option<CachedRender>,
}

impl PdfView {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
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

        Ok(Self {
            display_list,
            page_bounds,
            page_width,
            page_height,
            cached: None,
        })
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
            let device = Device::from_pixmap(&mut pixmap)?;
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
}

impl Render for PdfView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();

        div().size_full().bg(rgb(0x202020)).child(
            canvas(
                move |bounds, window, cx| {
                    view.update(cx, |this, _| {
                        this.paint_data(bounds.size.width, window.scale_factor())
                            .expect("failed to render PDF")
                    })
                },
                |bounds, paint_data, window, _| {
                    let image_height = bounds.size.width * paint_data.aspect_ratio;
                    let image_bounds =
                        Bounds::new(bounds.origin, size(bounds.size.width, image_height));
                    let _ = window.paint_image(
                        image_bounds,
                        Corners::default(),
                        paint_data.image,
                        0,
                        false,
                    );
                },
            )
            .size_full(),
        )
    }
}
