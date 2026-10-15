//! Headless zoom benchmark for GPU vector page rendering.
//!
//! Captures one page through MuPDF, renders a scripted zoom from 0.25x to
//! 64x of the fit scale and back, and compares checkpoints with MuPDF's own
//! raster of the same view.
//!
//! cargo run --release -p glyphst-pdf-gpu --example zoom_bench -- <pdf> \
//!     [--page N] [--size WxH] [--frames N] [--zoom Z] [--focus X,Y] [--out DIR] [--images]

mod common;

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use common::{FORMAT, Gpu, LoadedPage, diff, ms, mupdf_view, save_png};
use glyphst_pdf_gpu::{Camera, Renderer};
use serde_json::json;

const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 64.0;
const CHECKPOINTS: [f32; 3] = [1.0, 8.0, 64.0];
/// The Reader's thumbnail scale and size limit.
const THUMBNAIL_SCALE: f32 = 0.35;
const THUMBNAIL_MAX_SIDE: u32 = 512;

struct Options {
    pdf: PathBuf,
    page: i32,
    size: [u32; 2],
    frames: usize,
    /// Repeats frames at this zoom instead of the scripted path.
    zoom: Option<f32>,
    /// Page point to zoom around; the page center by default.
    focus: Option<[f32; 2]>,
    out: Option<PathBuf>,
    images: bool,
}

fn parse_options() -> Result<Options> {
    let mut args = std::env::args().skip(1);
    let mut options = Options {
        pdf: PathBuf::new(),
        page: 0,
        size: [1920, 1080],
        frames: 240,
        zoom: None,
        focus: None,
        out: None,
        images: false,
    };
    while let Some(arg) = args.next() {
        let mut value = || args.next().context("missing option value");
        match arg.as_str() {
            "--page" => options.page = value()?.parse::<i32>()? - 1,
            "--size" => {
                let size = value()?;
                let (w, h) = size.split_once('x').context("size must be WxH")?;
                options.size = [w.parse()?, h.parse()?];
            }
            "--frames" => options.frames = value()?.parse()?,
            "--zoom" => options.zoom = Some(value()?.parse()?),
            "--focus" => {
                let focus = value()?;
                let (x, y) = focus.split_once(',').context("focus must be X,Y")?;
                options.focus = Some([x.parse()?, y.parse()?]);
            }
            "--out" => options.out = Some(value()?.into()),
            "--images" => options.images = true,
            _ if arg.starts_with("--") => bail!("unknown option {arg}"),
            _ => options.pdf = arg.into(),
        }
    }
    if options.pdf.as_os_str().is_empty() {
        bail!(
            "usage: zoom_bench <pdf> [--page N] [--size WxH] [--frames N] [--zoom Z] \
             [--focus X,Y] [--out DIR] [--images]"
        );
    }
    Ok(options)
}

fn percentile(sorted: &[Duration], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index].as_secs_f64() * 1000.0
}

fn main() -> Result<()> {
    let options = parse_options()?;
    let size = options.size;

    let loaded = LoadedPage::open(&options.pdf, options.page)?;
    let (list, vector_page, prepared) = (&loaded.list, &loaded.vector, &loaded.prepared);

    let gpu = Gpu::new(size)?;
    let renderer = Renderer::new(&gpu.device, FORMAT);
    let fit = loaded.fit_scale(size, 0);
    let uploaded = common::upload(&gpu.device, &gpu.queue, &renderer, &loaded, fit * 2.0)?;
    let gpu_page = &uploaded.page;
    let bounds = prepared.bounds;
    let focus = options
        .focus
        .unwrap_or([(bounds[0] + bounds[2]) / 2.0, (bounds[1] + bounds[3]) / 2.0]);

    let camera = |zoom: f32| Camera::centered(focus, fit * zoom, size);

    let started = Instant::now();
    gpu.render(&renderer, gpu_page, &camera(1.0));
    gpu.wait()?;
    let first_frame = started.elapsed();

    let mut frames = Vec::with_capacity(options.frames);
    let mut encodes = Vec::with_capacity(options.frames);
    // GPU time of each frame's pass, when the adapter has timestamps.
    let mut passes = Vec::with_capacity(options.frames);
    let half = options.frames.max(2) / 2;
    let ratio = (MAX_ZOOM / MIN_ZOOM).ln();
    for frame in 0..options.frames {
        let t = if frame < half {
            frame as f32 / (half - 1).max(1) as f32
        } else {
            1.0 - (frame - half) as f32 / (options.frames - half - 1).max(1) as f32
        };
        let zoom = options.zoom.unwrap_or(MIN_ZOOM * (ratio * t).exp());
        let started = Instant::now();
        let (stats, encode, pass) = gpu.render_timed(&renderer, gpu_page, &camera(zoom));
        gpu.wait()?;
        frames.push((zoom, started.elapsed(), stats));
        encodes.push((zoom, encode));
        passes.extend(pass.map(|pass| (zoom, pass)));
    }
    let mut sorted: Vec<Duration> = frames.iter().map(|(_, time, _)| *time).collect();
    sorted.sort();
    let mut gpu_sorted: Vec<Duration> = passes.iter().map(|(_, time)| *time).collect();
    gpu_sorted.sort();
    let mut ranked: Vec<_> = frames.iter().enumerate().collect();
    ranked.sort_by_key(|(_, (_, time, _))| std::cmp::Reverse(*time));
    let slowest: Vec<_> = ranked
        .iter()
        .take(5)
        .map(|(index, (zoom, time, stats))| {
            json!({
                "frame": index,
                "zoom": zoom,
                "level": gpu_page.level_for_scale(fit * zoom),
                "ms": ms(*time),
                "draw_calls": stats.draw_calls,
                "triangles": stats.triangles,
                "segments": stats.segments,
            })
        })
        .collect();
    let by_zoom: Vec<_> = [(0.25, 1.0), (1.0, 4.0), (4.0, 16.0), (16.0, 64.1)]
        .into_iter()
        .map(|(low, high)| {
            let mut times: Vec<Duration> = frames
                .iter()
                .filter(|(zoom, _, _)| *zoom >= low && *zoom < high)
                .map(|(_, time, _)| *time)
                .collect();
            times.sort();
            let mut encode: Vec<Duration> = encodes
                .iter()
                .filter(|(zoom, _)| *zoom >= low && *zoom < high)
                .map(|(_, time)| *time)
                .collect();
            encode.sort();
            let mut gpu: Vec<Duration> = passes
                .iter()
                .filter(|(zoom, _)| *zoom >= low && *zoom < high)
                .map(|(_, time)| *time)
                .collect();
            gpu.sort();
            json!({
                "zoom": [low, high],
                "p50_ms": percentile(&times, 0.5),
                "max_ms": percentile(&times, 1.0),
                "encode_p50_ms": percentile(&encode, 0.5),
                "encode_max_ms": percentile(&encode, 1.0),
                "gpu_p50_ms": percentile(&gpu, 0.5),
                "gpu_max_ms": percentile(&gpu, 1.0),
            })
        })
        .collect();

    let name = options
        .pdf
        .file_stem()
        .map(|stem| stem.to_string_lossy().replace(' ', "_"))
        .unwrap_or_default();
    if let Some(out) = &options.out {
        std::fs::create_dir_all(out)?;
    }
    let mut checkpoints = Vec::new();
    for zoom in CHECKPOINTS.into_iter().chain(options.zoom) {
        let camera = camera(zoom);
        gpu.render(&renderer, gpu_page, &camera);
        let vector = gpu.read_target()?;
        let started = Instant::now();
        let reference = mupdf_view(list, &camera)?;
        let mupdf_ms = ms(started.elapsed());
        let result = diff(&vector, &reference, size);
        if options.images
            && let Some(out) = &options.out
        {
            let stem = format!("{name}-{zoom}x");
            save_png(&out.join(format!("{stem}-gpu.png")), vector, size)?;
            save_png(&out.join(format!("{stem}-mupdf.png")), reference, size)?;
            save_png(&out.join(format!("{stem}-diff.png")), result.image, size)?;
        }
        checkpoints.push(json!({
            "zoom": zoom,
            "level": gpu_page.level_for_scale(camera.scale),
            "mupdf_view_ms": mupdf_ms,
            "mean_abs_diff": result.mean_abs,
            "differing_pixels": result.differing,
        }));
    }

    // The sidebar thumbnail the Reader draws from a resident page.
    let thumbnail = renderer.render_thumbnail(
        &gpu.device,
        &gpu.queue,
        gpu_page,
        THUMBNAIL_SCALE,
        THUMBNAIL_MAX_SIDE,
    );
    let thumbnail_size = [thumbnail.width(), thumbnail.height()];
    let thumbnail_pixels = gpu.read_texture(&thumbnail)?;
    let reference = mupdf_view(list, &Camera::fitting(bounds, thumbnail_size))?;
    let result = diff(&thumbnail_pixels, &reference, thumbnail_size);
    if options.images
        && let Some(out) = &options.out
    {
        save_png(
            &out.join(format!("{name}-thumbnail-gpu.png")),
            thumbnail_pixels,
            thumbnail_size,
        )?;
        save_png(
            &out.join(format!("{name}-thumbnail-mupdf.png")),
            reference,
            thumbnail_size,
        )?;
    }
    let thumbnail = json!({
        "size": thumbnail_size,
        "mean_abs_diff": result.mean_abs,
        "differing_pixels": result.differing,
    });

    let stats = &vector_page.stats;
    let summary = json!({
        "pdf": options.pdf.display().to_string(),
        "page": options.page + 1,
        "adapter": gpu.adapter,
        "viewport": size,
        "capture": {
            "operations": stats.operations,
            "fills": stats.fills,
            "strokes": stats.strokes,
            "glyphs": stats.glyphs,
            "path_points": stats.path_points,
            "raster_layers": stats.raster_layers,
            "raster_ops": stats
                .raster_ops
                .iter()
                .map(|(reason, count)| (format!("{reason:?}"), *count))
                .collect::<std::collections::BTreeMap<_, _>>(),
        },
        "prepared": {
            "levels": prepared.levels.iter().map(|level| json!({
                "max_scale": level.max_scale,
                "fill_triangles": level.fill_indices.len() / 3,
                "segments": level.segments.len(),
                "batches": level.batches.len(),
                "failed_fills": level.failed_fills,
            })).collect::<Vec<_>>(),
            "fit_scale": fit,
            "overview_scale": gpu_page.overview_scale(),
            "gpu_mib": gpu_page.gpu_bytes as f64 / (1024.0 * 1024.0),
            "raster_layer_megapixels": uploaded.layer_pixels as f64 / 1e6,
        },
        "timings_ms": {
            "parse_display_list": ms(loaded.parse),
            "capture": ms(loaded.capture),
            "prepare": ms(loaded.prepare),
            "upload": ms(uploaded.upload),
            "raster_layers": ms(uploaded.raster_layers),
            "overview": ms(uploaded.overview),
            "first_frame": ms(first_frame),
        },
        "zoom_frames": {
            "count": frames.len(),
            "range": [MIN_ZOOM, MAX_ZOOM],
            "p50_ms": percentile(&sorted, 0.5),
            "p95_ms": percentile(&sorted, 0.95),
            "max_ms": percentile(&sorted, 1.0),
            "gpu_p50_ms": percentile(&gpu_sorted, 0.5),
            "gpu_p95_ms": percentile(&gpu_sorted, 0.95),
            "slowest": slowest,
            "by_zoom": by_zoom,
        },
        "checkpoints": checkpoints,
        "thumbnail": thumbnail,
    });
    let text = serde_json::to_string_pretty(&summary)?;
    println!("{text}");
    if let Some(out) = &options.out {
        std::fs::write(out.join(format!("{name}.json")), text)?;
    }
    Ok(())
}
