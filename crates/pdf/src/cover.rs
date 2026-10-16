//! A document's first page as a small picture, for the covers home and the
//! new-tab launcher show. MuPDF draws it on white, `width` pixels across.

use std::path::Path;

use anyhow::{Context as _, Result};
use mupdf::{Colorspace, Matrix};

use crate::GlyphstDocument;

/// A rendered first page: RGB rows, three bytes a pixel.
#[derive(Debug)]
pub struct Cover {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// Renders the first page of the document at `path`, `width` pixels wide
/// (a page much taller than wide is cut to twice its width).
pub fn render_cover(path: &Path, width: u32) -> Result<Cover> {
    let document = GlyphstDocument::open(path)?;
    let page = document
        .inner()
        .load_page(0)
        .with_context(|| format!("{} has no first page", path.display()))?;
    let bounds = page.bounds()?;
    let scale = width as f32 / bounds.width().max(1.0);
    let pixmap = page.to_pixmap(
        &Matrix::new_scale(scale, scale),
        &Colorspace::device_rgb(),
        false,
        true,
    )?;
    let (w, h) = (pixmap.width(), pixmap.height());
    let n = usize::from(pixmap.n());
    let stride = usize::try_from(pixmap.stride())?;
    anyhow::ensure!(n >= 3 && w > 0 && h > 0, "unexpected pixmap");
    let rows = h.min(w * 2);
    let samples = pixmap.samples();
    let mut rgb = Vec::with_capacity(w as usize * rows as usize * 3);
    for row in 0..rows as usize {
        let line = &samples[row * stride..row * stride + w as usize * n];
        for pixel in line.chunks_exact(n) {
            rgb.extend_from_slice(&pixel[..3]);
        }
    }
    Ok(Cover {
        width: w,
        height: rows,
        rgb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_page_renders_at_the_width_asked() {
        let path = std::env::temp_dir().join(format!("glyphst-cover-{}.pdf", std::process::id()));
        std::fs::write(&path, crate::test_pdf::text_pdf(&["cover"])).unwrap();
        let cover = render_cover(&path, 120).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(cover.width, 120);
        assert!(cover.height > 0);
        assert_eq!(cover.rgb.len(), (cover.width * cover.height * 3) as usize);
    }
}
