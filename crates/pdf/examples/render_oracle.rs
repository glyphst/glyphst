#![forbid(unsafe_code)]

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use mupdf::{Colorspace, Device, Document, ImageFormat, Matrix, Pixmap, Rect};

fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let input = arguments
        .next()
        .context("usage: render_oracle <input.pdf> <output.png> [page] [scale]")?;
    let output = arguments
        .next()
        .context("usage: render_oracle <input.pdf> <output.png> [page] [scale]")?;
    let page_index = arguments
        .next()
        .map(|value| value.to_string_lossy().parse::<i32>())
        .transpose()
        .context("page must be a non-negative integer")?
        .unwrap_or(0);
    let scale = arguments
        .next()
        .map(|value| value.to_string_lossy().parse::<f32>())
        .transpose()
        .context("scale must be a finite positive number")?
        .unwrap_or(2.0);
    if arguments.next().is_some() {
        bail!("usage: render_oracle <input.pdf> <output.png> [page] [scale]");
    }
    if page_index < 0 {
        bail!("page must be a non-negative integer");
    }
    if !scale.is_finite() || scale <= 0.0 {
        bail!("scale must be a finite positive number");
    }

    let input = path_text(&input, "input")?;
    let output = path_text(&output, "output")?;
    let document =
        Document::open(input).with_context(|| format!("failed to open PDF {}", input.display()))?;
    let page = document
        .load_page(page_index)
        .with_context(|| format!("failed to load PDF page {}", page_index + 1))?;
    let bounds = page.bounds().context("failed to read PDF page bounds")?;
    if !finite_rect(bounds) || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        bail!("PDF page has invalid bounds: {bounds:?}");
    }

    let matrix = Matrix::new_scale(scale, scale);
    let pixel_bounds = bounds.transform(&matrix).round();
    if pixel_bounds.is_empty() {
        bail!("scaled PDF page has empty pixel bounds");
    }
    let mut pixmap = Pixmap::new_with_rect(&Colorspace::device_rgb(), pixel_bounds, true)
        .context("failed to allocate MuPDF oracle pixmap")?;
    pixmap
        .clear_with(255)
        .context("failed to clear MuPDF oracle pixmap")?;
    let device = Device::from_pixmap(&pixmap).context("failed to create MuPDF raster device")?;
    page.run(&device, &matrix)
        .context("failed to render PDF page with MuPDF")?;
    page.run_annotations(&device, &matrix)
        .context("failed to render PDF annotations with MuPDF")?;
    drop(device);
    pixmap
        .save_as(
            output.to_str().context("output path is not valid UTF-8")?,
            ImageFormat::PNG,
        )
        .with_context(|| format!("failed to write oracle image {}", output.display()))?;

    println!(
        "rendered page {} at {:.3}x to {} ({}x{})",
        page_index + 1,
        scale,
        output.display(),
        pixmap.width(),
        pixmap.height()
    );
    Ok(())
}

fn path_text<'a>(value: &'a std::ffi::OsStr, label: &str) -> Result<&'a Path> {
    let path = Path::new(value);
    if path.as_os_str().is_empty() {
        bail!("{label} path must not be empty");
    }
    Ok(path)
}

fn finite_rect(rect: Rect) -> bool {
    [rect.x0, rect.y0, rect.x1, rect.y1]
        .into_iter()
        .all(f32::is_finite)
}
