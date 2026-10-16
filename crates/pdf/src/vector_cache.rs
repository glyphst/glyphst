//! Captured pages kept on disk, so a dense page opened again skips MuPDF.
//!
//! Each file holds one [`VectorPage`] of one PDF page. Its header names the
//! document (canonical path, size and modification time), the page and the
//! program that wrote it (the executable's size and modification time), so
//! a changed document, a rebuilt Reader or a different MuPDF never reads a
//! stale capture: any mismatch is a miss, and the next capture of that page
//! replaces it. Files are written whole under a temporary name and renamed
//! into place; unreadable ones are removed. Draws are stored in segments
//! decoded on all cores. The least recently used files go once the
//! directory holds more than its limit.

use std::{
    collections::HashMap,
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    io::Write as _,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};

use crate::vector::{
    CaptureStats, ImagePixels, LineCap, PagePath, PageRect, PathVerb, StrokeStyle, VectorDraw,
    VectorItem, VectorPage,
};

const MAGIC: &[u8; 8] = b"GLYPHSTV";
/// Bumped whenever the encoding or [`VectorPage`] changes.
const FORMAT: u32 = 2;
/// Draws per independently decoded segment.
const SEGMENT_DRAWS: usize = 1 << 16;

/// The page cache of one document.
#[derive(Debug)]
pub struct PageCache {
    dir: PathBuf,
    max_bytes: u64,
    document: PathBuf,
    /// The document's and the program's identity, as written in headers.
    identity: Vec<u8>,
}

impl PageCache {
    /// The cache of the document at `path`, in `dir`, kept under
    /// `max_bytes`; `None` when the document or the running program cannot
    /// be identified.
    pub fn open(dir: PathBuf, path: &Path, max_bytes: u64) -> Option<Self> {
        let document = fs::canonicalize(path).ok()?;
        let program = std::env::current_exe().ok()?;
        let mut identity = Writer::default();
        identity.bytes(document.as_os_str().as_encoded_bytes());
        for file in [&document, &program] {
            let metadata = fs::metadata(file).ok()?;
            identity.u64(metadata.len());
            let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
            identity.u64(modified.as_secs());
            identity.u32(modified.subsec_nanos());
        }
        Some(Self {
            dir,
            max_bytes,
            document,
            identity: identity.0,
        })
    }

    /// One file per document page, so a newer capture (of a changed
    /// document, or by another build) replaces the older one.
    fn file(&self, page: usize) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        self.document.hash(&mut hasher);
        page.hash(&mut hasher);
        self.dir.join(format!("{:016x}.bin", hasher.finish()))
    }

    fn header(&self, page: usize) -> Vec<u8> {
        let mut header = Writer::default();
        header.0.extend_from_slice(MAGIC);
        header.u32(FORMAT);
        header.bytes(&self.identity);
        header.u64(page as u64);
        header.0
    }

    /// The capture of `page`, when one was stored for this document and
    /// program. Reading it marks it recently used.
    pub fn load(&self, page: usize) -> Option<VectorPage> {
        let file = self.file(page);
        let data = fs::read(&file).ok()?;
        let header = self.header(page);
        if !data.starts_with(&header) {
            // An older capture, replaced once this one is stored.
            return None;
        }
        match decode(&data[header.len()..]) {
            Some(page) => {
                let _ = fs::File::options()
                    .write(true)
                    .open(&file)
                    .and_then(|file| file.set_modified(SystemTime::now()));
                Some(page)
            }
            None => {
                let _ = fs::remove_file(&file);
                None
            }
        }
    }

    /// Stores the capture of `page`, then removes the oldest captures over
    /// the limit. Pages with raster layers are not stored: their layers are
    /// rasterized from a display list, which is made with the capture.
    pub fn store(&self, page: usize, vector: &VectorPage) -> Result<()> {
        let body = encode(vector).context("page has raster layers")?;
        create_private_directory(&self.dir)?;
        let file = self.file(page);
        let partial = file.with_extension("partial");
        let mut options = fs::File::options();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut out = options
            .open(&partial)
            .with_context(|| format!("failed to create {}", partial.display()))?;
        out.write_all(&self.header(page))
            .and_then(|()| out.write_all(&body))
            .with_context(|| format!("failed to write {}", partial.display()))?;
        drop(out);
        fs::rename(&partial, &file)
            .with_context(|| format!("failed to move {} into place", file.display()))?;
        evict(&self.dir, self.max_bytes);
        Ok(())
    }
}

/// Removes the least recently used captures in `dir` until it holds at most
/// `max_bytes`.
fn evict(dir: &Path, max_bytes: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            (path.extension()? == "bin").then_some(())?;
            let metadata = entry.metadata().ok()?;
            Some((metadata.modified().ok()?, metadata.len(), path))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, size, path) in files {
        if total <= max_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
}

fn create_private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .with_context(|| format!("failed to create {}", path.display()))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
    }
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn f32(&mut self, value: f32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.u64(bytes.len() as u64);
        self.0.extend_from_slice(bytes);
    }

    fn rect(&mut self, rect: PageRect) {
        rect.into_iter().for_each(|value| self.f32(value));
    }

    fn path(&mut self, path: &PagePath) {
        self.u32(path.verbs.len() as u32);
        for verb in &path.verbs {
            self.u8(match verb {
                PathVerb::MoveTo => 0,
                PathVerb::LineTo => 1,
                PathVerb::CubicTo => 2,
                PathVerb::Close => 3,
            });
        }
        self.u32(path.points.len() as u32);
        for [x, y] in &path.points {
            self.f32(*x);
            self.f32(*y);
        }
    }

    fn cap(&mut self, cap: LineCap) {
        self.u8(match cap {
            LineCap::Butt => 0,
            LineCap::Round => 1,
            LineCap::Square => 2,
        });
    }
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.data.split_at_checked(len)?;
        self.data = rest;
        Some(taken)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.array()?))
    }

    fn len(&mut self) -> Option<usize> {
        usize::try_from(self.u64()?).ok()
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.array()?))
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.len()?;
        self.take(len)
    }

    fn rect(&mut self) -> Option<PageRect> {
        Some([self.f32()?, self.f32()?, self.f32()?, self.f32()?])
    }

    fn path(&mut self) -> Option<PagePath> {
        let verbs = self.u32()? as usize;
        let verbs = self
            .take(verbs)?
            .iter()
            .map(|verb| match verb {
                0 => Some(PathVerb::MoveTo),
                1 => Some(PathVerb::LineTo),
                2 => Some(PathVerb::CubicTo),
                3 => Some(PathVerb::Close),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let points = self.u32()? as usize;
        let points = self
            .take(points.checked_mul(8)?)?
            .chunks_exact(8)
            .map(|point| {
                let value = |at: usize| f32::from_le_bytes(point[at..at + 4].try_into().unwrap());
                [value(0), value(4)]
            })
            .collect();
        Some(PagePath { verbs, points })
    }

    fn cap(&mut self) -> Option<LineCap> {
        match self.u8()? {
            0 => Some(LineCap::Butt),
            1 => Some(LineCap::Round),
            2 => Some(LineCap::Square),
            _ => None,
        }
    }
}

const FILL: u8 = 0;
const STROKE: u8 = 1;
const IMAGE: u8 = 2;
const CLIP_PUSH: u8 = 3;
const CLIP_POP: u8 = 4;

/// The page's bounds, stats, its distinct clips and stroke styles, then its
/// draws referring to them; `None` for a page with raster layers.
fn encode(page: &VectorPage) -> Option<Vec<u8>> {
    let mut clips: HashMap<[u32; 4], u32> = HashMap::new();
    let mut clip_table = Writer::default();
    let mut styles: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut style_table = Writer::default();
    let mut segments: Vec<(usize, Writer)> = Vec::new();
    for (index, draw) in page.draws.iter().enumerate() {
        if index % SEGMENT_DRAWS == 0 {
            segments.push((0, Writer::default()));
        }
        let (count, draws) = segments.last_mut().expect("a segment was started");
        *count += 1;
        let clip = match draw.clip {
            None => 0,
            Some(rect) => {
                let next = clips.len() as u32 + 1;
                *clips.entry(rect.map(f32::to_bits)).or_insert_with(|| {
                    clip_table.rect(rect);
                    next
                })
            }
        };
        match &draw.item {
            VectorItem::Fill {
                path,
                even_odd,
                color,
            } => {
                draws.u8(FILL);
                draws.u32(clip);
                draws.u8(u8::from(*even_odd));
                draws.0.extend_from_slice(color);
                draws.path(path);
            }
            VectorItem::Stroke { path, style, color } => {
                let mut encoded = Writer::default();
                encoded.f32(style.width);
                encoded.cap(style.start_cap);
                encoded.cap(style.end_cap);
                encoded.cap(style.dash_cap);
                encoded.u32(style.dashes.len() as u32);
                style.dashes.iter().for_each(|dash| encoded.f32(*dash));
                encoded.f32(style.dash_phase);
                let next = styles.len() as u32;
                let index = *styles.entry(encoded.0).or_insert_with_key(|encoded| {
                    style_table.0.extend_from_slice(encoded);
                    next
                });
                draws.u8(STROKE);
                draws.u32(clip);
                draws.u32(index);
                draws.0.extend_from_slice(color);
                draws.path(path);
            }
            VectorItem::Image {
                rgba,
                size,
                transform,
                interpolate,
            } => {
                draws.u8(IMAGE);
                draws.u32(clip);
                draws.u32(size[0]);
                draws.u32(size[1]);
                transform.iter().for_each(|value| draws.f32(*value));
                draws.u8(u8::from(*interpolate));
                draws.bytes(&rgba.0);
            }
            VectorItem::ClipPush { path, even_odd } => {
                draws.u8(CLIP_PUSH);
                draws.u32(clip);
                draws.u8(u8::from(*even_odd));
                draws.path(path);
            }
            VectorItem::ClipPop => {
                draws.u8(CLIP_POP);
                draws.u32(clip);
            }
            VectorItem::Raster { .. } => return None,
        }
    }
    let stats = &page.stats;
    let mut out = Writer::default();
    out.rect(page.bounds);
    out.u32(stats.operations);
    for count in [
        stats.fills,
        stats.strokes,
        stats.glyphs,
        stats.images,
        stats.path_points,
    ] {
        out.u64(count as u64);
    }
    out.u32(clips.len() as u32);
    out.0.extend_from_slice(&clip_table.0);
    out.u32(styles.len() as u32);
    out.0.extend_from_slice(&style_table.0);
    out.u32(segments.len() as u32);
    for (count, draws) in &segments {
        out.u64(*count as u64);
        out.u64(draws.0.len() as u64);
    }
    for (_, draws) in &segments {
        out.0.extend_from_slice(&draws.0);
    }
    Some(out.0)
}

fn decode(data: &[u8]) -> Option<VectorPage> {
    let mut input = Reader { data };
    let bounds = input.rect()?;
    let operations = input.u32()?;
    let mut count = || usize::try_from(input.u64()?).ok();
    let (fills, strokes, glyphs, images, path_points) =
        (count()?, count()?, count()?, count()?, count()?);
    let clips = (0..input.u32()?)
        .map(|_| input.rect())
        .collect::<Option<Vec<_>>>()?;
    let styles = (0..input.u32()?)
        .map(|_| {
            let width = input.f32()?;
            let (start_cap, end_cap, dash_cap) = (input.cap()?, input.cap()?, input.cap()?);
            let dashes = (0..input.u32()?)
                .map(|_| input.f32())
                .collect::<Option<Vec<_>>>()?;
            Some(StrokeStyle {
                width,
                start_cap,
                end_cap,
                dash_cap,
                dashes,
                dash_phase: input.f32()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let segments = (0..input.u32()?)
        .map(|_| Some((input.len()?, input.len()?)))
        .collect::<Option<Vec<_>>>()?;
    let mut parts = Vec::with_capacity(segments.len());
    for (count, len) in segments {
        parts.push((count, input.take(len)?));
    }
    if !input.data.is_empty() {
        return None;
    }
    // Segments decode on all cores, in contiguous groups.
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    let group = parts.len().div_ceil(workers).max(1);
    let decoded: Vec<Option<Vec<VectorDraw>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = parts
            .chunks(group)
            .map(|group| {
                let (clips, styles) = (&clips, &styles);
                scope.spawn(move || {
                    let mut draws = Vec::new();
                    for &(count, data) in group {
                        decode_draws(data, count, clips, styles, &mut draws)?;
                    }
                    Some(draws)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().ok().flatten())
            .collect()
    });
    let mut draws = Vec::with_capacity(decoded.iter().flatten().map(Vec::len).sum());
    for group in decoded {
        draws.append(&mut group?);
    }
    Some(VectorPage {
        bounds,
        draws,
        stats: CaptureStats {
            operations,
            fills,
            strokes,
            glyphs,
            images,
            path_points,
            raster_layers: 0,
            raster_ops: Vec::new(),
        },
    })
}

/// Decodes `count` draws from `data`, which must hold exactly them, onto
/// `draws`.
fn decode_draws(
    data: &[u8],
    count: usize,
    clips: &[PageRect],
    styles: &[StrokeStyle],
    draws: &mut Vec<VectorDraw>,
) -> Option<()> {
    let mut input = Reader { data };
    // Each draw takes at least five bytes; a bogus count fails below rather
    // than in a huge allocation.
    draws.reserve(count.min(data.len() / 5));
    for _ in 0..count {
        let tag = input.u8()?;
        let clip = match input.u32()? {
            0 => None,
            index => Some(*clips.get(index as usize - 1)?),
        };
        let item = match tag {
            FILL => {
                let even_odd = input.u8()? != 0;
                let color = input.array()?;
                VectorItem::Fill {
                    path: input.path()?,
                    even_odd,
                    color,
                }
            }
            STROKE => {
                let style = styles.get(input.u32()? as usize)?.clone();
                let color = input.array()?;
                VectorItem::Stroke {
                    path: input.path()?,
                    style,
                    color,
                }
            }
            IMAGE => {
                let size = [input.u32()?, input.u32()?];
                let mut transform = [0.0; 6];
                for value in &mut transform {
                    *value = input.f32()?;
                }
                let interpolate = input.u8()? != 0;
                VectorItem::Image {
                    rgba: ImagePixels(input.bytes()?.into()),
                    size,
                    transform,
                    interpolate,
                }
            }
            CLIP_PUSH => {
                let even_odd = input.u8()? != 0;
                VectorItem::ClipPush {
                    path: input.path()?,
                    even_odd,
                }
            }
            CLIP_POP => VectorItem::ClipPop,
            _ => return None,
        };
        draws.push(VectorDraw { item, clip });
    }
    input.data.is_empty().then_some(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn page() -> VectorPage {
        let path = PagePath {
            verbs: vec![
                PathVerb::MoveTo,
                PathVerb::LineTo,
                PathVerb::CubicTo,
                PathVerb::Close,
            ],
            points: vec![[0.0, 0.0], [10.0, 0.5], [1.0, 2.0], [3.0, 4.0], [5.0, 6.0]],
        };
        let style = StrokeStyle {
            width: 0.5,
            start_cap: LineCap::Round,
            end_cap: LineCap::Butt,
            dash_cap: LineCap::Square,
            dashes: vec![2.0, 1.0],
            dash_phase: 0.5,
        };
        let draw = |item, clip| VectorDraw { item, clip };
        VectorPage {
            bounds: [0.0, 0.0, 612.0, 792.0],
            draws: vec![
                draw(
                    VectorItem::ClipPush {
                        path: path.clone(),
                        even_odd: true,
                    },
                    None,
                ),
                draw(
                    VectorItem::Fill {
                        path: path.clone(),
                        even_odd: false,
                        color: [1, 2, 3, 255],
                    },
                    Some([1.0, 2.0, 3.0, 4.0]),
                ),
                draw(
                    VectorItem::Stroke {
                        path: path.clone(),
                        style: style.clone(),
                        color: [9, 8, 7, 128],
                    },
                    Some([1.0, 2.0, 3.0, 4.0]),
                ),
                draw(
                    VectorItem::Stroke {
                        path,
                        style,
                        color: [0, 0, 0, 255],
                    },
                    None,
                ),
                draw(
                    VectorItem::Image {
                        rgba: ImagePixels(Arc::from(vec![1u8, 2, 3, 4])),
                        size: [1, 1],
                        transform: [1.0, 0.0, 0.0, 1.0, 5.0, 6.0],
                        interpolate: true,
                    },
                    None,
                ),
                draw(VectorItem::ClipPop, None),
            ],
            stats: CaptureStats {
                operations: 6,
                fills: 1,
                strokes: 2,
                glyphs: 0,
                images: 1,
                path_points: 15,
                ..CaptureStats::default()
            },
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "glyphst-page-cache-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn pages_round_trip() {
        let page = page();
        assert_eq!(decode(&encode(&page).unwrap()), Some(page));
    }

    #[test]
    fn pages_with_raster_layers_are_not_encoded() {
        let mut page = page();
        page.draws.push(VectorDraw {
            item: VectorItem::Raster {
                ops: 0..1,
                bounds: [0.0; 4],
            },
            clip: None,
        });
        assert!(encode(&page).is_none());
    }

    #[test]
    fn truncated_data_does_not_decode() {
        let data = encode(&page()).unwrap();
        for len in [0, 10, data.len() / 2, data.len() - 1] {
            assert!(decode(&data[..len]).is_none(), "{len} bytes");
        }
    }

    #[test]
    fn captures_are_found_for_their_document_only() {
        let dir = temp_dir("identity");
        fs::create_dir_all(&dir).unwrap();
        let document = dir.join("a.pdf");
        fs::write(&document, b"%PDF-1.7 one").unwrap();
        let cache = PageCache::open(dir.join("pages"), &document, u64::MAX).unwrap();
        assert!(cache.load(0).is_none());
        cache.store(0, &page()).unwrap();
        assert_eq!(cache.load(0), Some(page()));
        assert!(cache.load(1).is_none(), "another page");

        // A changed document is another document.
        fs::write(&document, b"%PDF-1.7 two, longer").unwrap();
        let changed = PageCache::open(dir.join("pages"), &document, u64::MAX).unwrap();
        assert!(changed.load(0).is_none());

        // A damaged capture is a miss and is removed.
        let file = cache.file(0);
        let data = fs::read(&file).unwrap();
        fs::write(&file, &data[..data.len() - 3]).unwrap();
        assert!(cache.load(0).is_none());
        assert!(!file.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_oldest_captures_go_over_the_limit() {
        let dir = temp_dir("evict");
        fs::create_dir_all(&dir).unwrap();
        let document = dir.join("a.pdf");
        fs::write(&document, b"%PDF-1.7").unwrap();
        let size = (encode(&page()).unwrap().len() + 200) as u64;
        // Room for two captures.
        let cache = PageCache::open(dir.join("pages"), &document, 2 * size).unwrap();
        cache.store(0, &page()).unwrap();
        cache.store(1, &page()).unwrap();
        // Page 0 is read, so page 1 is now the oldest.
        let old = SystemTime::now() - std::time::Duration::from_secs(60);
        for page in [0, 1] {
            fs::File::options()
                .write(true)
                .open(cache.file(page))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        assert!(cache.load(0).is_some());
        cache.store(2, &page()).unwrap();
        assert!(cache.load(0).is_some());
        assert!(cache.load(1).is_none());
        assert!(cache.load(2).is_some());
        fs::remove_dir_all(&dir).unwrap();
    }
}
