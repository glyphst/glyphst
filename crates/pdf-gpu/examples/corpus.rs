//! Corpus check for GPU vector page rendering.
//!
//! Renders page 1 of each PDF on the GPU at fit and 4x zoom and compares it
//! with MuPDF's raster of the same view. Each file runs in a child process
//! with a time limit, so a crash or hang is reported for that file only.
//! Files MuPDF cannot open, or cannot parse within the time limit, are
//! counted separately; they test the parser, not this renderer.
//!
//! cargo run --release -p glyphst-pdf-gpu --example corpus -- <dir> \
//!     [--limit N] [--timeout SECONDS] [--size WxH] [--rotate 0|90|180|270] [--out DIR]
//!
//! `--one FILE --images DIR` checks one file in this process and saves each
//! view's GPU render, MuPDF raster and difference as PNGs.
//!
//! Exits non-zero when a child crashes or times out after parsing.

mod common;

use std::{
    collections::BTreeMap,
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use common::{FORMAT, Gpu, LoadedPage, diff, ms, mupdf_view, save_png};
use glyphst_pdf_gpu::{Camera, Renderer};
use serde_json::{Value, json};

const ZOOMS: [f32; 2] = [1.0, 4.0];
/// Printed by a child once MuPDF has parsed the page.
const PARSED_MARKER: &str = "parsed";
/// Pages differing from MuPDF on more than this share of pixels are listed.
const REPORT_DIFFERING: f64 = 0.01;

struct Options {
    dir: PathBuf,
    one: Option<PathBuf>,
    limit: usize,
    timeout: Duration,
    size: [u32; 2],
    /// Quarter turns clockwise of every compared view.
    rotation: u8,
    out: Option<PathBuf>,
    /// With `--one`: where to save the views.
    images: Option<PathBuf>,
}

fn parse_options() -> Result<Options> {
    let mut args = std::env::args().skip(1);
    let mut options = Options {
        dir: PathBuf::new(),
        one: None,
        limit: 100,
        timeout: Duration::from_secs(30),
        size: [1024, 768],
        rotation: 0,
        out: None,
        images: None,
    };
    while let Some(arg) = args.next() {
        let mut value = || args.next().context("missing option value");
        match arg.as_str() {
            "--one" => options.one = Some(value()?.into()),
            "--limit" => options.limit = value()?.parse()?,
            "--timeout" => options.timeout = Duration::from_secs(value()?.parse()?),
            "--size" => {
                let size = value()?;
                let (w, h) = size.split_once('x').context("size must be WxH")?;
                options.size = [w.parse()?, h.parse()?];
            }
            "--rotate" => {
                options.rotation = match value()?.as_str() {
                    "0" => 0,
                    "90" => 1,
                    "180" => 2,
                    "270" => 3,
                    other => bail!("--rotate takes 0, 90, 180 or 270, not {other}"),
                };
            }
            "--out" => options.out = Some(value()?.into()),
            "--images" => options.images = Some(value()?.into()),
            _ if arg.starts_with("--") => bail!("unknown option {arg}"),
            _ => options.dir = arg.into(),
        }
    }
    if options.one.is_none() && options.dir.as_os_str().is_empty() {
        bail!(
            "usage: corpus <dir> [--limit N] [--timeout SECONDS] [--size WxH] [--rotate DEGREES] [--out DIR]"
        );
    }
    Ok(options)
}

/// Child mode: one file, one JSON line on stdout.
fn check_one(path: &Path, size: [u32; 2], rotation: u8, images: Option<&Path>) -> Value {
    let loaded = match LoadedPage::open_with(path, 0, || {
        println!("{PARSED_MARKER}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }) {
        Ok(loaded) => loaded,
        Err(error) => return json!({ "status": "unreadable", "error": format!("{error:#}") }),
    };
    match render_and_compare(&loaded, size, rotation, images) {
        Ok(mut result) => {
            let stats = &loaded.vector.stats;
            result["status"] = json!("ok");
            result["capture"] = json!({
                "operations": stats.operations,
                "fills": stats.fills,
                "strokes": stats.strokes,
                "glyphs": stats.glyphs,
                "raster_layers": stats.raster_layers,
                "raster_ops": stats
                    .raster_ops
                    .iter()
                    .map(|(reason, count)| (format!("{reason:?}"), *count))
                    .collect::<BTreeMap<_, _>>(),
            });
            result["capture_ms"] = json!(ms(loaded.capture));
            result["prepare_ms"] = json!(ms(loaded.prepare));
            result
        }
        Err(error) => json!({ "status": "error", "error": format!("{error:#}") }),
    }
}

fn render_and_compare(
    loaded: &LoadedPage,
    size: [u32; 2],
    rotation: u8,
    images: Option<&Path>,
) -> Result<Value> {
    let gpu = Gpu::new(size)?;
    let renderer = Renderer::new(&gpu.device, FORMAT);
    let fit = loaded.fit_scale(size, rotation);
    // Raster layers at the largest compared zoom, so the comparison checks
    // routing and order rather than layer resolution.
    let uploaded = common::upload(
        &gpu.device,
        &gpu.queue,
        &renderer,
        loaded,
        fit * ZOOMS[ZOOMS.len() - 1],
    )?;
    let [x0, y0, x1, y1] = loaded.prepared.bounds;
    let focus = [(x0 + x1) / 2.0, (y0 + y1) / 2.0];
    let mut views = Vec::new();
    for zoom in ZOOMS {
        let camera = Camera::centered_rotated(focus, fit * zoom, size, rotation);
        let started = Instant::now();
        gpu.render(&renderer, &uploaded.page, &camera);
        gpu.wait()?;
        let frame = started.elapsed();
        let vector = gpu.read_target()?;
        let reference = mupdf_view(&loaded.list, &camera)?;
        let result = diff(&vector, &reference, size);
        if let Some(dir) = images {
            std::fs::create_dir_all(dir)?;
            let stem = format!("{}x", zoom);
            save_png(&dir.join(format!("{stem}-gpu.png")), vector, size)?;
            save_png(&dir.join(format!("{stem}-mupdf.png")), reference, size)?;
            save_png(&dir.join(format!("{stem}-diff.png")), result.image, size)?;
        }
        views.push(json!({
            "zoom": zoom,
            "frame_ms": ms(frame),
            "differing_pixels": result.differing,
            "mean_abs_diff": result.mean_abs,
        }));
    }
    Ok(json!({ "views": views, "gpu_mib": uploaded.page.gpu_bytes as f64 / 1048576.0 }))
}

/// Up to `limit` PDFs from `dir`, evenly spaced in name order.
fn select(dir: &Path, limit: usize) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
        })
        .collect();
    files.sort();
    if files.len() <= limit || limit == 0 {
        return Ok(files);
    }
    Ok((0..limit)
        .map(|index| files[index * files.len() / limit].clone())
        .collect())
}

fn run_child(exe: &Path, file: &Path, options: &Options) -> Value {
    let mut child = match Command::new(exe)
        .arg("--one")
        .arg(file)
        .arg("--size")
        .arg(format!("{}x{}", options.size[0], options.size[1]))
        .arg("--rotate")
        .arg((u32::from(options.rotation) * 90).to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return json!({ "status": "crash", "error": error.to_string() }),
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                let parsed = stdout
                    .lines()
                    .last()
                    .and_then(|line| serde_json::from_str::<Value>(line).ok());
                return match parsed {
                    Some(value) if status.success() => value,
                    _ => {
                        json!({ "status": "crash", "error": format!("child exited with {status}") })
                    }
                };
            }
            Ok(None) if started.elapsed() > options.timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let mut stdout = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                let parsed = stdout.lines().any(|line| line == PARSED_MARKER);
                return json!({ "status": if parsed { "timeout" } else { "parser_timeout" } });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => return json!({ "status": "crash", "error": error.to_string() }),
        }
    }
}

fn main() -> Result<()> {
    let options = parse_options()?;
    if let Some(file) = &options.one {
        println!(
            "{}",
            check_one(
                file,
                options.size,
                options.rotation,
                options.images.as_deref()
            )
        );
        return Ok(());
    }

    let files = select(&options.dir, options.limit)?;
    let exe = std::env::current_exe()?;
    let started = Instant::now();
    let mut results = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let file_started = Instant::now();
        let mut result = run_child(&exe, file, &options);
        result["file"] = json!(
            file.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        );
        result["seconds"] = json!(file_started.elapsed().as_secs_f64());
        eprintln!(
            "[{}/{}] {} {}",
            index + 1,
            files.len(),
            result["status"].as_str().unwrap_or("?"),
            result["file"].as_str().unwrap_or("?")
        );
        results.push(result);
    }

    let mut statuses = BTreeMap::<String, usize>::new();
    let mut raster_ops = BTreeMap::<String, u64>::new();
    let mut pages_with_raster = 0;
    let mut differing = Vec::new();
    for result in &results {
        *statuses
            .entry(result["status"].as_str().unwrap_or("?").to_string())
            .or_default() += 1;
        if let Some(ops) = result["capture"]["raster_ops"].as_object() {
            if !ops.is_empty() {
                pages_with_raster += 1;
            }
            for (reason, count) in ops {
                *raster_ops.entry(reason.clone()).or_default() += count.as_u64().unwrap_or(0);
            }
        }
        let worst = result["views"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|view| view["differing_pixels"].as_f64())
            .fold(0.0, f64::max);
        if worst > REPORT_DIFFERING {
            differing.push((worst, result["file"].clone()));
        }
    }
    differing.sort_by(|a, b| b.0.total_cmp(&a.0));
    // How far the rendered files are from MuPDF at each zoom, per channel
    // value: a lower median means closer edges (antialiasing) overall.
    let mean_abs_diff: BTreeMap<String, Value> = ZOOMS
        .iter()
        .map(|&zoom| {
            let mut values: Vec<f64> = results
                .iter()
                .flat_map(|result| result["views"].as_array().into_iter().flatten())
                .filter(|view| view["zoom"].as_f64() == Some(f64::from(zoom)))
                .filter_map(|view| view["mean_abs_diff"].as_f64())
                .collect();
            values.sort_by(f64::total_cmp);
            let at = |share: f64| {
                values
                    .get(((values.len() as f64 - 1.0) * share).round() as usize)
                    .copied()
            };
            (
                format!("zoom_{zoom}"),
                json!({ "median": at(0.5), "p90": at(0.9) }),
            )
        })
        .collect();
    let failures =
        statuses.get("crash").copied().unwrap_or(0) + statuses.get("timeout").copied().unwrap_or(0);
    let summary = json!({
        "dir": options.dir.display().to_string(),
        "rotation": u32::from(options.rotation) * 90,
        "files": results.len(),
        "seconds": started.elapsed().as_secs_f64(),
        "statuses": statuses,
        "pages_with_raster_layers": pages_with_raster,
        "raster_ops": raster_ops,
        "mean_abs_diff": mean_abs_diff,
        "differing_over_1_percent": differing
            .iter()
            .map(|(share, file)| json!({ "file": file, "differing_pixels": share }))
            .collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&summary)?;
    println!("{text}");
    if let Some(out) = &options.out {
        std::fs::create_dir_all(out)?;
        std::fs::write(out.join("summary.json"), text)?;
        let lines: Vec<String> = results.iter().map(Value::to_string).collect();
        std::fs::write(out.join("results.jsonl"), lines.join("\n") + "\n")?;
    }
    if failures > 0 {
        bail!("{failures} file(s) crashed or timed out");
    }
    Ok(())
}
