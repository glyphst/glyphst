//! The wallpaper image backgrounds can draw: decoded, fitted and
//! mipmapped on a worker thread, then handed to the GPU on a later frame.
//! The worker also finds the colour a generated theme is built on.

use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver},
};

/// The longest side a wallpaper keeps; larger images are scaled down.
pub const MAX_SIDE: u32 = 3840;
/// The longest side of the copy its seed colour is found in, as
/// Material's own image path does.
const SEED_SIDE: u32 = 128;

/// A wallpaper ready for the GPU: premultiplied RGBA8 and its mip chain.
pub struct WallpaperImage {
    pub rgba: Vec<u8>,
    pub size: [u32; 2],
    pub mips: Vec<(Vec<u8>, [u32; 2])>,
    /// The colour a theme is best built on (`None` without opaque pixels).
    pub seed: Option<[u8; 3]>,
}

/// A wallpaper being read.
pub struct Loading {
    pub path: String,
    receiver: Receiver<Result<WallpaperImage, String>>,
}

impl Loading {
    /// Starts reading `path`; `ctx` repaints when it is ready.
    pub fn start(path: String, ctx: &egui::Context) -> Self {
        let (sender, receiver) = mpsc::channel();
        let (file, ctx) = (PathBuf::from(&path), ctx.clone());
        let spawned = std::thread::Builder::new()
            .name("wallpaper".into())
            .spawn(move || {
                let _ = sender.send(read(&file));
                ctx.request_repaint();
            });
        if let Err(error) = spawned {
            let (sender, failed) = mpsc::channel();
            let _ = sender.send(Err(error.to_string()));
            return Self {
                path,
                receiver: failed,
            };
        }
        Self { path, receiver }
    }

    /// The image (or why it could not be read), once ready.
    pub fn take(&self) -> Option<Result<WallpaperImage, String>> {
        self.receiver.try_recv().ok()
    }
}

fn read(path: &std::path::Path) -> Result<WallpaperImage, String> {
    let image = image::open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .to_rgba8();
    let (width, height) = image.dimensions();
    let [fit_width, fit_height] = fit([width, height], MAX_SIDE);
    let image = if [fit_width, fit_height] == [width, height] {
        image
    } else {
        image::imageops::resize(
            &image,
            fit_width,
            fit_height,
            image::imageops::FilterType::Triangle,
        )
    };
    let [seed_width, seed_height] = fit([fit_width, fit_height], SEED_SIDE);
    let small = image::imageops::thumbnail(&image, seed_width, seed_height);
    let seed = glyphst_ui::scheme::seed_from_pixels(small.as_raw());
    let mut rgba = image.into_raw();
    premultiply(&mut rgba);
    let size = [fit_width, fit_height];
    let mips = glyphst_pdf_gpu::prepare::mip_chain(&rgba, size);
    Ok(WallpaperImage {
        rgba,
        size,
        mips,
        seed,
    })
}

/// `size` scaled down to fit `max` on its longer side, at least 1×1.
pub fn fit(size: [u32; 2], max: u32) -> [u32; 2] {
    let longest = size[0].max(size[1]);
    if longest <= max {
        return [size[0].max(1), size[1].max(1)];
    }
    let scale = f64::from(max) / f64::from(longest);
    size.map(|side| ((f64::from(side) * scale).round() as u32).max(1))
}

/// Multiplies each pixel's colour by its alpha, as effects expect.
pub fn premultiply(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_images_fit_and_pixels_are_premultiplied() {
        assert_eq!(fit([7680, 4320], MAX_SIDE), [3840, 2160]);
        assert_eq!(fit([1000, 5000], 2500), [500, 2500]);
        assert_eq!(fit([800, 600], MAX_SIDE), [800, 600]);
        assert_eq!(fit([0, 10], 4), [1, 4]);
        let mut pixels = [200, 100, 50, 128, 10, 20, 30, 255, 99, 99, 99, 0];
        premultiply(&mut pixels);
        assert_eq!(pixels, [100, 50, 25, 128, 10, 20, 30, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn a_missing_file_is_reported() {
        let error = read(std::path::Path::new("/nonexistent/wallpaper.png")).err();
        assert!(error.is_some_and(|error| error.contains("wallpaper.png")));
    }
}
