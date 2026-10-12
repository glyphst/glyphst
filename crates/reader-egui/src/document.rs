//! Continuous page layout, the view camera, and background page loading.

use std::{
    collections::{HashSet, VecDeque},
    ops::Range,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use glyphst_pdf::{
    GlyphstDocument,
    vector::{LayerImage, PageRect, capture_display_list, rasterize_layer},
};
use glyphst_pdf_gpu::{Camera, PreparedPage};
use mupdf::{Colorspace, DisplayList, Matrix};

/// Gap between pages, in page points.
pub const PAGE_GAP: f64 = 12.0;
/// Device pixels per point of the loading thumbnails.
const THUMBNAIL_SCALE: f32 = 0.35;
const THUMBNAIL_MAX_POINTS: usize = 200_000;
/// Device pixels per point at which raster layers are rasterized.
const LAYER_SCALE: f32 = 4.0;
const MAX_LAYER_DIMENSION: u32 = 4096;

/// Pages stacked vertically and centered on the widest one. Document
/// coordinates are page points with the origin at the first page's top-left
/// corner; they are kept in f64 so long documents stay exact at deep zoom.
pub struct Layout {
    pub bounds: Vec<PageRect>,
    /// Document position of each page's top-left corner.
    pub offsets: Vec<[f64; 2]>,
    pub width: f64,
    pub height: f64,
}

impl Layout {
    pub fn new(bounds: Vec<PageRect>) -> Self {
        let width = bounds
            .iter()
            .map(|b| f64::from(b[2] - b[0]))
            .fold(0.0, f64::max);
        let mut y = 0.0;
        let offsets = bounds
            .iter()
            .map(|b| {
                let offset = [(width - f64::from(b[2] - b[0])) / 2.0, y];
                y += f64::from(b[3] - b[1]) + PAGE_GAP;
                offset
            })
            .collect();
        Self {
            bounds,
            offsets,
            width,
            height: (y - PAGE_GAP).max(0.0),
        }
    }

    pub fn len(&self) -> usize {
        self.bounds.len()
    }

    pub fn page_size(&self, page: usize) -> [f64; 2] {
        let b = self.bounds[page];
        [f64::from(b[2] - b[0]), f64::from(b[3] - b[1])]
    }

    /// Pages overlapping the document rows `y0..y1`.
    pub fn visible(&self, y0: f64, y1: f64) -> Range<usize> {
        let first = self
            .offsets
            .partition_point(|offset| offset[1] < y0)
            .saturating_sub(1);
        let mut end = first;
        while end < self.len() && self.offsets[end][1] <= y1 {
            end += 1;
        }
        let first = (first..end)
            .find(|&page| self.offsets[page][1] + self.page_size(page)[1] >= y0)
            .unwrap_or(end);
        first..end
    }

    /// The renderer camera drawing `page` for a view whose top-left corner
    /// shows document point `origin` at `scale` device pixels per point.
    pub fn page_camera(
        &self,
        page: usize,
        origin: [f64; 2],
        scale: f32,
        viewport: [u32; 2],
    ) -> Camera {
        let b = self.bounds[page];
        let offset = self.offsets[page];
        Camera {
            origin: [
                (origin[0] - offset[0] + f64::from(b[0])) as f32,
                (origin[1] - offset[1] + f64::from(b[1])) as f32,
            ],
            scale,
            viewport,
        }
    }
}

/// Exponential approach rate of the animated zoom, per second.
const ZOOM_RESPONSE: f64 = 18.0;

/// Scroll position and animated zoom of a view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// Document point at the view's top-left corner.
    pub origin: [f64; 2],
    /// Device pixels per page point.
    pub scale: f64,
    pub target_scale: f64,
    /// View pixel kept fixed while the zoom animates.
    pub anchor: [f64; 2],
}

impl View {
    pub fn new(scale: f64) -> Self {
        Self {
            origin: [0.0; 2],
            scale,
            target_scale: scale,
            anchor: [0.0; 2],
        }
    }

    pub fn zoom_by(&mut self, factor: f64, anchor: [f64; 2], min: f64, max: f64) {
        self.anchor = anchor;
        self.target_scale = (self.target_scale * factor).clamp(min, max);
    }

    pub fn scroll_by_pixels(&mut self, dx: f64, dy: f64) {
        self.origin[0] += dx / self.scale;
        self.origin[1] += dy / self.scale;
    }

    /// Advances the zoom animation; returns whether it is still running.
    pub fn animate(&mut self, seconds: f64) -> bool {
        if (self.target_scale / self.scale - 1.0).abs() < 1e-4 {
            self.set_scale(self.target_scale);
            return false;
        }
        let step = 1.0 - (-ZOOM_RESPONSE * seconds.min(1.0 / 30.0)).exp();
        let scale = (self.scale.ln() + (self.target_scale.ln() - self.scale.ln()) * step).exp();
        self.set_scale(scale);
        true
    }

    /// Sets the scale immediately, keeping the anchor fixed on screen.
    pub fn set_scale(&mut self, scale: f64) {
        let [ax, ay] = self.anchor;
        let page = [
            self.origin[0] + ax / self.scale,
            self.origin[1] + ay / self.scale,
        ];
        self.scale = scale;
        self.origin = [page[0] - ax / scale, page[1] - ay / scale];
    }

    /// Keeps the view within the document, centering it when narrower.
    pub fn clamp(&mut self, layout: &Layout, viewport: [f64; 2]) {
        let [width, height] = [viewport[0] / self.scale, viewport[1] / self.scale];
        let margin = PAGE_GAP;
        self.origin[0] = if width >= layout.width + 2.0 * margin {
            (layout.width - width) / 2.0
        } else {
            self.origin[0].clamp(-margin, layout.width + margin - width)
        };
        self.origin[1] = if height >= layout.height + 2.0 * margin {
            (layout.height - height) / 2.0
        } else {
            self.origin[1].clamp(-margin, layout.height + margin - height)
        };
    }
}

/// A page result from the loader.
pub enum Loaded {
    Thumbnail {
        page: usize,
        image: egui::ColorImage,
    },
    Prepared {
        page: usize,
        prepared: Box<PreparedPage>,
        layers: Vec<(usize, LayerImage)>,
        seconds: f64,
    },
    Failed {
        page: usize,
        error: String,
    },
    /// A raster layer re-rasterized for the current view.
    Layer {
        page: usize,
        layer: usize,
        image: LayerImage,
    },
}

/// Work for the loader thread.
enum Request {
    Page(usize),
    Layer {
        page: usize,
        layer: usize,
        ops: Range<u32>,
        rect: PageRect,
        scale: f32,
    },
}

/// Display lists the loader keeps for re-rasterizing layers.
const CACHED_LISTS: usize = 8;

/// Captures and prepares pages on a worker thread with its own document.
/// Requests for pages no longer wanted are skipped.
pub struct Loader {
    requests: Sender<Request>,
    results: Receiver<Loaded>,
    wanted: Arc<Mutex<HashSet<usize>>>,
    requested: HashSet<usize>,
}

impl Loader {
    pub fn new(path: PathBuf, ctx: egui::Context) -> Self {
        let (requests, request_rx) = mpsc::channel::<Request>();
        let (result_tx, results) = mpsc::channel();
        let wanted = Arc::new(Mutex::new(HashSet::new()));
        let worker_wanted = wanted.clone();
        thread::Builder::new()
            .name("page-loader".into())
            .spawn(move || {
                let document = match GlyphstDocument::open(&path) {
                    Ok(document) => document,
                    Err(error) => {
                        let _ = result_tx.send(Loaded::Failed {
                            page: 0,
                            error: format!("{error:#}"),
                        });
                        return;
                    }
                };
                let mut lists: VecDeque<(usize, DisplayList)> = VecDeque::new();
                let mut send = |loaded| {
                    let sent = result_tx.send(loaded).is_ok();
                    ctx.request_repaint();
                    sent
                };
                while let Ok(request) = request_rx.recv() {
                    match request {
                        Request::Page(page) => {
                            if !worker_wanted
                                .lock()
                                .is_ok_and(|wanted| wanted.contains(&page))
                            {
                                continue;
                            }
                            if let Some(list) = load_page(document.inner(), page, &mut send) {
                                lists.retain(|(cached, _)| *cached != page);
                                lists.push_back((page, list));
                                if lists.len() > CACHED_LISTS {
                                    lists.pop_front();
                                }
                            }
                        }
                        Request::Layer {
                            page,
                            layer,
                            ops,
                            rect,
                            scale,
                        } => {
                            let image = rasterize_cached(
                                document.inner(),
                                &mut lists,
                                page,
                                ops,
                                rect,
                                scale,
                            );
                            if let Some(image) = image
                                && !send(Loaded::Layer { page, layer, image })
                            {
                                return;
                            }
                        }
                    }
                }
            })
            .expect("failed to start the page loader");
        Self {
            requests,
            results,
            wanted,
            requested: HashSet::new(),
        }
    }

    /// Requests `pages` (in priority order) and forgets other pages that
    /// were requested but not loaded yet.
    pub fn want(&mut self, pages: &[usize], loaded: impl Fn(usize) -> bool) {
        if let Ok(mut wanted) = self.wanted.lock() {
            wanted.clear();
            wanted.extend(pages.iter().copied().filter(|&page| !loaded(page)));
        }
        self.requested.retain(|page| pages.contains(page));
        for &page in pages {
            if !loaded(page) && self.requested.insert(page) {
                let _ = self.requests.send(Request::Page(page));
            }
        }
    }

    /// Re-rasterizes raster layer `layer` of `page` (its display-list `ops`)
    /// over page area `rect` at `scale` device pixels per point.
    pub fn rasterize_layer(
        &self,
        page: usize,
        layer: usize,
        ops: Range<u32>,
        rect: PageRect,
        scale: f32,
    ) {
        let _ = self.requests.send(Request::Layer {
            page,
            layer,
            ops,
            rect,
            scale,
        });
    }

    /// Marks `page` as no longer loaded, so it can be requested again.
    pub fn forget(&mut self, page: usize) {
        self.requested.remove(&page);
    }

    pub fn try_recv(&self) -> Option<Loaded> {
        self.results.try_recv().ok()
    }
}

/// Loads one page, sending a thumbnail as soon as it is drawn and the
/// prepared vectors after. MuPDF interprets the page once, into a display
/// list both are made from. Pages with more path points than
/// [`THUMBNAIL_MAX_POINTS`] get no thumbnail: rasterizing them takes
/// seconds, about as long as preparing their vectors.
fn load_page(
    document: &mupdf::Document,
    page: usize,
    send: &mut dyn FnMut(Loaded) -> bool,
) -> Option<DisplayList> {
    let started = Instant::now();
    let result = (|| -> Result<Option<(Loaded, DisplayList)>> {
        let list = document
            .load_page(i32::try_from(page)?)
            .with_context(|| format!("failed to load page {}", page + 1))?
            .to_display_list(true)?;
        let vector = capture_display_list(&list)?;
        if vector.stats.path_points <= THUMBNAIL_MAX_POINTS {
            let thumbnail = list.to_pixmap(
                &Matrix::new_scale(THUMBNAIL_SCALE, THUMBNAIL_SCALE),
                &Colorspace::device_rgb(),
                false,
            )?;
            let size = [thumbnail.width() as usize, thumbnail.height() as usize];
            let image = egui::ColorImage::from_rgb(size, thumbnail.samples());
            if !send(Loaded::Thumbnail { page, image }) {
                return Ok(None);
            }
        }
        let prepared = PreparedPage::new(&vector);
        let mut layers = Vec::new();
        for (index, layer) in prepared.raster_layers.iter().enumerate() {
            if let Some(image) = rasterize_layer(
                &list,
                layer.ops.clone(),
                layer.bounds,
                LAYER_SCALE,
                MAX_LAYER_DIMENSION,
            )? {
                layers.push((index, image));
            }
        }
        let loaded = Loaded::Prepared {
            page,
            prepared: Box::new(prepared),
            layers,
            seconds: started.elapsed().as_secs_f64(),
        };
        Ok(Some((loaded, list)))
    })();
    match result {
        Ok(Some((loaded, list))) => send(loaded).then_some(list),
        Ok(None) => None,
        Err(error) => {
            send(Loaded::Failed {
                page,
                error: format!("{error:#}"),
            });
            None
        }
    }
}

/// Rasterizes layer operations from a cached display list, loading the
/// page's list again when it was dropped.
fn rasterize_cached(
    document: &mupdf::Document,
    lists: &mut VecDeque<(usize, DisplayList)>,
    page: usize,
    ops: Range<u32>,
    rect: PageRect,
    scale: f32,
) -> Option<LayerImage> {
    if !lists.iter().any(|(cached, _)| *cached == page) {
        let list = document
            .load_page(i32::try_from(page).ok()?)
            .ok()?
            .to_display_list(true)
            .ok()?;
        lists.push_back((page, list));
        if lists.len() > CACHED_LISTS {
            lists.pop_front();
        }
    }
    let (_, list) = lists.iter().find(|(cached, _)| *cached == page)?;
    rasterize_layer(list, ops, rect, scale, MAX_LAYER_DIMENSION)
        .ok()
        .flatten()
}

/// One entry of a document's outline, flattened in reading order.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineEntry {
    pub title: String,
    pub page: Option<usize>,
    pub depth: usize,
}

/// What a tab needs before its first frame, read on the calling thread.
pub struct DocumentInfo {
    pub bounds: Vec<PageRect>,
    pub outline: Vec<OutlineEntry>,
    pub title: Option<String>,
}

impl DocumentInfo {
    pub fn read(path: &std::path::Path) -> Result<Self> {
        let document = GlyphstDocument::open(path)?;
        let document = document.inner();
        let count = document.page_count()?;
        let bounds = (0..count)
            .map(|index| {
                let b = document.load_page(index)?.bounds()?;
                Ok([b.x0, b.y0, b.x1, b.y1])
            })
            .collect::<Result<Vec<_>>>()?;
        let mut outline = Vec::new();
        // A broken outline should not stop the document from opening.
        if let Ok(entries) = document.outlines() {
            flatten_outline(&entries, 0, &mut outline);
        }
        let title = document
            .metadata(mupdf::MetadataName::Title)
            .ok()
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty());
        Ok(Self {
            bounds,
            outline,
            title,
        })
    }
}

fn flatten_outline(entries: &[mupdf::Outline], depth: usize, out: &mut Vec<OutlineEntry>) {
    for entry in entries {
        out.push(OutlineEntry {
            title: entry.title.trim().to_string(),
            page: entry
                .dest
                .as_ref()
                .map(|dest| dest.loc.page_number as usize),
            depth,
        });
        flatten_outline(&entry.down, depth + 1, out);
    }
}

/// Rolling frame intervals for the HUD and the benchmark.
#[derive(Default)]
pub struct FrameTimes {
    intervals: Vec<Duration>,
    last: Option<Instant>,
}

impl FrameTimes {
    pub fn tick(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last.replace(now) {
            self.intervals.push(now - last);
            if self.intervals.len() > 240 {
                self.intervals.remove(0);
            }
        }
    }

    /// Forgets the interval spanning an idle period.
    pub fn idle(&mut self) {
        self.last = None;
    }

    pub fn percentile_ms(&self, p: f64) -> f64 {
        percentile_ms(&self.intervals, p)
    }
}

pub fn percentile_ms(values: &[Duration], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted[((sorted.len() - 1) as f64 * p).round() as usize].as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::new(vec![
            [0.0, 0.0, 600.0, 800.0],
            [0.0, 0.0, 400.0, 300.0],
            [0.0, 0.0, 600.0, 800.0],
        ])
    }

    #[test]
    fn pages_stack_with_gaps_and_center_on_the_widest() {
        let layout = layout();
        assert_eq!(layout.offsets, [[0.0, 0.0], [100.0, 812.0], [0.0, 1124.0]]);
        assert_eq!(layout.height, 1924.0);
    }

    #[test]
    fn visible_pages_cover_the_rows_shown() {
        let layout = layout();
        assert_eq!(layout.visible(0.0, 100.0), 0..1);
        assert!(
            layout.visible(805.0, 811.0).is_empty(),
            "only the gap is visible"
        );
        assert_eq!(layout.visible(700.0, 900.0), 0..2);
        assert_eq!(layout.visible(1200.0, 5000.0), 2..3);
    }

    #[test]
    fn page_cameras_map_document_points_to_view_pixels() {
        let layout = layout();
        let camera = layout.page_camera(1, [100.0, 812.0], 2.0, [800, 600]);
        assert_eq!(
            camera.origin,
            [0.0, 0.0],
            "page 1's corner is the view's corner"
        );
        let far = Layout::new(vec![[0.0, 0.0, 600.0, 800.0]; 400]);
        let camera = far.page_camera(399, [0.5, 399.0 * 812.0 + 0.25], 128.0, [800, 600]);
        assert_eq!(
            camera.origin,
            [0.5, 0.25],
            "deep pages keep sub-point precision"
        );
    }

    #[test]
    fn zoom_animation_keeps_the_anchor_fixed_and_converges() {
        let mut view = View::new(1.0);
        view.origin = [10.0, 20.0];
        view.zoom_by(4.0, [100.0, 50.0], 0.1, 100.0);
        let anchor_page = [10.0 + 100.0, 20.0 + 50.0];
        for _ in 0..200 {
            view.animate(1.0 / 60.0);
            let shown = [
                view.origin[0] + 100.0 / view.scale,
                view.origin[1] + 50.0 / view.scale,
            ];
            assert!(
                (shown[0] - anchor_page[0]).abs() < 1e-9
                    && (shown[1] - anchor_page[1]).abs() < 1e-9
            );
        }
        assert_eq!(view.scale, 4.0);
        assert!(!view.animate(1.0 / 60.0));
    }

    #[test]
    fn clamping_centers_small_documents_and_bounds_large_ones() {
        let layout = layout();
        let mut view = View::new(0.1);
        view.clamp(&layout, [1000.0, 1000.0]);
        assert_eq!(
            view.origin,
            [(600.0 - 10000.0) / 2.0, (1924.0 - 10000.0) / 2.0]
        );
        let mut view = View::new(2.0);
        view.origin = [-500.0, 5000.0];
        view.clamp(&layout, [400.0, 400.0]);
        assert_eq!(view.origin, [-PAGE_GAP, 1924.0 + PAGE_GAP - 200.0]);
    }
}
