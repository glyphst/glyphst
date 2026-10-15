//! Shared headless helpers for the pdf-gpu examples: a GPU target, page
//! loading and upload, and comparison with MuPDF's raster of the same view.
#![allow(dead_code)]

use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use glyphst_pdf::vector::{VectorPage, capture_display_list, rasterize_layer};
use glyphst_pdf_gpu::{Camera, DrawStats, GpuPage, OffscreenTarget, PreparedPage, Renderer};
use mupdf::{Colorspace, Device, DisplayList, Matrix, Pixmap, Rect};

pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// A pixel differs when no pixel within one pixel in the other image is
/// within this channel distance, which tolerates antialiasing phase.
pub const DIFF_THRESHOLD: u8 = 64;
const MAX_LAYER_DIMENSION: u32 = 4096;

pub fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// One page opened through MuPDF, captured and prepared.
pub struct LoadedPage {
    pub list: DisplayList,
    pub vector: VectorPage,
    pub prepared: PreparedPage,
    pub parse: Duration,
    pub capture: Duration,
    pub prepare: Duration,
}

impl LoadedPage {
    pub fn open(path: &Path, page: i32) -> Result<Self> {
        Self::open_with(path, page, || {})
    }

    /// Like [`LoadedPage::open`], calling `parsed` once MuPDF has built the
    /// display list.
    pub fn open_with(path: &Path, page: i32, parsed: impl FnOnce()) -> Result<Self> {
        let started = Instant::now();
        let document = mupdf::Document::open(path.to_str().context("non-UTF-8 path")?)?;
        let list = document.load_page(page)?.to_display_list(true)?;
        let parse = started.elapsed();
        parsed();
        let started = Instant::now();
        let vector = capture_display_list(&list)?;
        let capture = started.elapsed();
        let started = Instant::now();
        let prepared = PreparedPage::new(&vector);
        let prepare = started.elapsed();
        Ok(Self {
            list,
            vector,
            prepared,
            parse,
            capture,
            prepare,
        })
    }

    /// Device pixels per page point that fit the page, turned `rotation`
    /// quarter turns, into `size`.
    pub fn fit_scale(&self, size: [u32; 2], rotation: u8) -> f32 {
        let [x0, y0, x1, y1] = glyphst_pdf_gpu::rotate_rect(self.prepared.bounds, rotation);
        (size[0] as f32 / (x1 - x0)).min(size[1] as f32 / (y1 - y0))
    }
}

pub struct Uploaded {
    pub page: GpuPage,
    pub upload: Duration,
    pub raster_layers: Duration,
    pub layer_pixels: u64,
    pub overview: Duration,
}

/// Uploads `loaded`, rasterizes its raster layers at `layer_scale` device
/// pixels per point over their bounds, and builds an overview when the page
/// wants one.
pub fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    loaded: &LoadedPage,
    layer_scale: f32,
) -> Result<Uploaded> {
    let wait = || {
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map(|_| ())
            .map_err(|error| anyhow::anyhow!("GPU wait failed: {error}"))
    };
    let started = Instant::now();
    let mut page = renderer.upload(device, queue, &loaded.prepared);
    wait()?;
    let upload = started.elapsed();

    // A reader re-rasterizes the visible area after zooming settles.
    let started = Instant::now();
    let mut layer_pixels = 0u64;
    for (index, layer) in loaded.prepared.raster_layers.iter().enumerate() {
        let Some(image) = rasterize_layer(
            &loaded.list,
            layer.ops.clone(),
            layer.bounds,
            layer_scale,
            MAX_LAYER_DIMENSION,
        )?
        else {
            continue;
        };
        renderer.set_raster_layer(
            device,
            queue,
            &mut page,
            index,
            image.rect,
            &image.rgba,
            image.size,
        );
        let [width, height] = image.size;
        layer_pixels += u64::from(width) * u64::from(height);
    }
    wait()?;
    let raster_layers = started.elapsed();

    let started = Instant::now();
    if page.wants_overview() {
        let dimension = device.limits().max_texture_dimension_2d.min(8192);
        renderer.build_overview(device, queue, &mut page, dimension);
    }
    wait()?;
    let overview = started.elapsed();
    Ok(Uploaded {
        page,
        upload,
        raster_layers,
        layer_pixels,
        overview,
    })
}

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: String,
    pub target: OffscreenTarget,
}

impl Gpu {
    pub fn new(size: [u32; 2]) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .context("no GPU adapter")?;
        let info = adapter.get_info();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("pdf_gpu_example"),
                required_limits: adapter.limits(),
                ..Default::default()
            }))?;
        let target = OffscreenTarget::new(&device, FORMAT, size, wgpu::TextureUsages::COPY_SRC);
        Ok(Self {
            device,
            queue,
            adapter: format!("{} ({:?})", info.name, info.backend),
            target,
        })
    }

    pub fn wait(&self) -> Result<()> {
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map(|_| ())
            .map_err(|error| anyhow::anyhow!("GPU wait failed: {error}"))
    }

    pub fn render(&self, renderer: &Renderer, page: &GpuPage, camera: &Camera) -> DrawStats {
        self.render_timed(renderer, page, camera).0
    }

    /// Renders and returns how long [`Renderer::draw`] took on the CPU
    /// (selecting and recording the draws).
    pub fn render_timed(
        &self,
        renderer: &Renderer,
        page: &GpuPage,
        camera: &Camera,
    ) -> (DrawStats, std::time::Duration) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let (stats, encode) = {
            let mut pass = self.target.begin_pass(&mut encoder, wgpu::Color::WHITE);
            let started = std::time::Instant::now();
            let stats = renderer.draw(&self.queue, page, camera, &mut pass);
            (stats, started.elapsed())
        };
        self.queue.submit([encoder.finish()]);
        (stats, encode)
    }

    /// The target's pixels as RGB8 rows.
    pub fn read_target(&self) -> Result<Vec<u8>> {
        let [width, height] = self.target.size();
        let padded = (width * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            self.target.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        buffer.map_async(wgpu::MapMode::Read, .., move |result| {
            let _ = sender.send(result);
        });
        self.wait()?;
        receiver.recv()??;
        let mapped = buffer.get_mapped_range(..)?;
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for row in mapped.chunks_exact(padded as usize) {
            for pixel in row[..(width * 4) as usize].chunks_exact(4) {
                rgb.extend_from_slice(&pixel[..3]);
            }
        }
        Ok(rgb)
    }
}

pub fn mupdf_view(list: &DisplayList, camera: &Camera) -> Result<Vec<u8>> {
    let [width, height] = camera.viewport;
    let mut pixmap = Pixmap::new_with_w_h(
        &Colorspace::device_rgb(),
        width as i32,
        height as i32,
        false,
    )?;
    pixmap.clear_with(255)?;
    let device = Device::from_pixmap(&pixmap)?;
    // pixel = (axes * page - origin) * scale, as the GPU camera maps it.
    let s = camera.scale;
    let [m00, m01, m10, m11] = camera.axes();
    let ctm = Matrix::new(
        m00 * s,
        m10 * s,
        m01 * s,
        m11 * s,
        -camera.origin[0] * s,
        -camera.origin[1] * s,
    );
    list.run(
        &device,
        &ctm,
        Rect::new(0.0, 0.0, width as f32, height as f32),
    )?;
    drop(device);
    Ok(pixmap.samples().to_vec())
}

pub struct Diff {
    pub mean_abs: f64,
    pub differing: f64,
    pub image: Vec<u8>,
}

pub fn diff(a: &[u8], b: &[u8], size: [u32; 2]) -> Diff {
    let [width, height] = [size[0] as usize, size[1] as usize];
    let pixel = |image: &[u8], x: usize, y: usize| {
        let offset = (y * width + x) * 3;
        [image[offset], image[offset + 1], image[offset + 2]]
    };
    let distance = |p: [u8; 3], q: [u8; 3]| (0..3).map(|c| p[c].abs_diff(q[c])).max().unwrap_or(0);
    let unmatched = |from: &[u8], to: &[u8], x: usize, y: usize| {
        let p = pixel(from, x, y);
        let mut best = u8::MAX;
        for ny in y.saturating_sub(1)..=(y + 1).min(height - 1) {
            for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                best = best.min(distance(p, pixel(to, nx, ny)));
            }
        }
        best > DIFF_THRESHOLD
    };
    let mut sum = 0u64;
    let mut differing = 0usize;
    let mut image = vec![255u8; width * height * 3];
    for y in 0..height {
        for x in 0..width {
            sum += u64::from(distance(pixel(a, x, y), pixel(b, x, y)));
            if unmatched(a, b, x, y) || unmatched(b, a, x, y) {
                differing += 1;
                let offset = (y * width + x) * 3;
                image[offset..offset + 3].copy_from_slice(&[255, 0, 0]);
            }
        }
    }
    let pixels = (width * height) as f64;
    Diff {
        mean_abs: sum as f64 / pixels,
        differing: differing as f64 / pixels,
        image,
    }
}

pub fn save_png(path: &Path, rgb: Vec<u8>, size: [u32; 2]) -> Result<()> {
    image::RgbImage::from_raw(size[0], size[1], rgb)
        .context("bad image size")?
        .save(path)
        .with_context(|| format!("failed to write {}", path.display()))
}
