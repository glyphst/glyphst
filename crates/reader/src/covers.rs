//! First-page covers for home and the new-tab launcher. A worker thread
//! renders each document's first page with MuPDF (`glyphst_pdf::
//! render_cover`) and keeps it as a JPEG in the cache directory's
//! `covers/`, named after the document's path, size and modification
//! time, so a changed document gets a new cover. Covers come back as
//! textures; a document that cannot be rendered (no first page, a
//! password, a format MuPDF cannot read) keeps its placeholder.

use std::{
    collections::HashMap,
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, Sender, channel},
    time::UNIX_EPOCH,
};

/// Width of a rendered cover in pixels: twice a 148pt card.
const WIDTH: u32 = 296;
const QUALITY: u8 = 85;

/// A cover's texture and its width over its height.
pub type CoverTexture = (egui::TextureId, f32);

enum Cover {
    Pending,
    /// The texture, its aspect and its colour (gamepad mode's glow).
    Ready(egui::TextureHandle, f32, Option<egui::Color32>),
    Failed,
}

pub struct Covers {
    covers: HashMap<PathBuf, Cover>,
    requests: Sender<PathBuf>,
    done: Receiver<(PathBuf, Option<egui::ColorImage>)>,
}

impl Covers {
    pub fn new(ctx: &egui::Context) -> Self {
        let (requests, wanted) = channel::<PathBuf>();
        let (finished, done) = channel();
        let ctx = ctx.clone();
        let dir = glyphst_core::paths::cache_dir().join("covers");
        let spawned = std::thread::Builder::new()
            .name("covers".into())
            .spawn(move || {
                for path in wanted {
                    let image = load_or_render(&dir, &path);
                    if finished.send((path, image)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
        if let Err(error) = spawned {
            log::warn!("no cover thread: {error}");
        }
        Self {
            covers: HashMap::new(),
            requests,
            done,
        }
    }

    /// Takes in the covers rendered since the last frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((path, image)) = self.done.try_recv() {
            let cover = match image {
                Some(image) => {
                    let aspect = image.size[0] as f32 / image.size[1].max(1) as f32;
                    let name = format!("cover:{}", path.display());
                    let tint = tint(&image);
                    Cover::Ready(
                        ctx.load_texture(name, image, Default::default()),
                        aspect,
                        tint,
                    )
                }
                None => Cover::Failed,
            };
            self.covers.insert(path, cover);
        }
    }

    /// Whether a cover asked for is still being rendered (screenshots
    /// wait for them).
    pub fn pending(&self) -> bool {
        self.covers
            .values()
            .any(|cover| matches!(cover, Cover::Pending))
    }

    /// The cover of `path`, asking for it the first time.
    pub fn get(&mut self, path: &Path) -> Option<CoverTexture> {
        match self.covers.get(path) {
            Some(Cover::Ready(texture, aspect, _)) => Some((texture.id(), *aspect)),
            Some(Cover::Pending | Cover::Failed) => None,
            None => {
                if self.requests.send(path.to_path_buf()).is_ok() {
                    self.covers.insert(path.to_path_buf(), Cover::Pending);
                }
                None
            }
        }
    }
}

impl Covers {
    /// The colour of `path`'s cover, once it is read: its pixels averaged
    /// by how colourful each is, so white pages and black text don't wash
    /// it out. `None` for a cover without colour.
    pub fn tint(&self, path: &Path) -> Option<egui::Color32> {
        match self.covers.get(path) {
            Some(Cover::Ready(_, _, tint)) => *tint,
            _ => None,
        }
    }
}

fn tint(image: &egui::ColorImage) -> Option<egui::Color32> {
    let (mut sum, mut weight) = ([0.0f64; 3], 0.0f64);
    for pixel in image.pixels.iter().step_by(7) {
        let [r, g, b, _] = pixel.to_array();
        let chroma = f64::from(r.max(g).max(b) - r.min(g).min(b)) / 255.0;
        let w = chroma * chroma;
        sum[0] += f64::from(r) * w;
        sum[1] += f64::from(g) * w;
        sum[2] += f64::from(b) * w;
        weight += w;
    }
    let count = (image.pixels.len() / 7).max(1) as f64;
    if weight / count < 0.01 {
        return None;
    }
    let channel = |c: f64| (c / weight).round().clamp(0.0, 255.0) as u8;
    Some(egui::Color32::from_rgb(
        channel(sum[0]),
        channel(sum[1]),
        channel(sum[2]),
    ))
}

/// The cached cover's file: the document's path, size and modification
/// time hashed.
fn cache_file(dir: &Path, path: &Path) -> Option<PathBuf> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    let mut hasher = DefaultHasher::new();
    (path, metadata.len(), modified, WIDTH).hash(&mut hasher);
    Some(dir.join(format!("{:016x}.jpg", hasher.finish())))
}

fn load_or_render(dir: &Path, path: &Path) -> Option<egui::ColorImage> {
    let file = cache_file(dir, path)?;
    if let Ok(image) = image::open(&file) {
        let rgb = image.to_rgb8();
        let size = [rgb.width() as usize, rgb.height() as usize];
        return Some(egui::ColorImage::from_rgb(size, rgb.as_raw()));
    }
    let cover = match glyphst_pdf::render_cover(path, WIDTH) {
        Ok(cover) => cover,
        Err(error) => {
            log::debug!("no cover for {}: {error:#}", path.display());
            return None;
        }
    };
    if fs::create_dir_all(dir).is_ok() {
        let saved = fs::File::create(&file)
            .map_err(image::ImageError::IoError)
            .and_then(|out| {
                image::codecs::jpeg::JpegEncoder::new_with_quality(out, QUALITY).encode(
                    &cover.rgb,
                    cover.width,
                    cover.height,
                    image::ExtendedColorType::Rgb8,
                )
            });
        if let Err(error) = saved {
            log::debug!("could not keep the cover of {}: {error}", path.display());
            let _ = fs::remove_file(&file);
        }
    }
    Some(egui::ColorImage::from_rgb(
        [cover.width as usize, cover.height as usize],
        &cover.rgb,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cover_is_rendered_once_and_then_read_from_the_cache() {
        let dir = std::env::temp_dir().join(format!("glyphst-covers-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("paper.pdf");
        fs::write(&pdf, glyphst_pdf::test_pdf::text_pdf(&["a cover"])).unwrap();
        let covers = dir.join("covers");
        let rendered = load_or_render(&covers, &pdf).unwrap();
        assert_eq!(rendered.size[0], WIDTH as usize);
        let file = cache_file(&covers, &pdf).unwrap();
        assert!(file.exists());
        let cached = load_or_render(&covers, &pdf).unwrap();
        assert_eq!(cached.size, rendered.size);
        assert!(load_or_render(&covers, &dir.join("missing.pdf")).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
