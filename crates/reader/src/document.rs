//! Continuous page layout, the view camera, and background page loading.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use glyphst_core::DocumentLayout;
use glyphst_pdf::{
    DocumentSnapshot, GlyphstDocument,
    text::{PageLink, PageText, page_links},
    vector::{LayerImage, PageRect, capture_display_list, rasterize_layer},
};
use glyphst_pdf_gpu::{Camera, PreparedPage, rotate, rotate_rect};
use mupdf::{Colorspace, DisplayList, Matrix};

use crate::generation::PageSketch;

/// Gap between pages, in page points.
pub const PAGE_GAP: f64 = 12.0;
/// Device pixels per point of the loading and sidebar thumbnails.
const THUMBNAIL_SCALE: f32 = 0.35;
/// A sidebar thumbnail slower than this marks the document as too dense
/// for thumbnails of pages that are not open.
const HEAVY_THUMBNAIL: Duration = Duration::from_millis(400);
const THUMBNAIL_MAX_POINTS: usize = 200_000;
/// Device pixels per point at which raster layers are rasterized.
const LAYER_SCALE: f32 = 4.0;
const MAX_LAYER_DIMENSION: u32 = 4096;

/// How pages are arranged: turned by quarter turns, and one or two to a
/// row (a book's spread, the first page alone).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Arrangement {
    /// Quarter turns clockwise (0-3).
    pub rotation: u8,
    pub spread: bool,
}

/// Pages in rows, one or two to a row, each row centered on the widest.
/// Document coordinates are page points with the origin at the first
/// page's top-left corner; they are kept in f64 so long documents stay
/// exact at deep zoom. A page turned by the arrangement takes the space of
/// its turned box.
pub struct Layout {
    /// Each page's box in its own (unturned) page space.
    pub bounds: Vec<PageRect>,
    /// Document position of each page's turned box's top-left corner.
    pub offsets: Vec<[f64; 2]>,
    pub width: f64,
    pub height: f64,
    pub arrangement: Arrangement,
    /// Each page's turned box.
    turned: Vec<PageRect>,
}

impl Layout {
    pub fn new(bounds: Vec<PageRect>) -> Self {
        Self::arranged(bounds, Arrangement::default())
    }

    pub fn arranged(bounds: Vec<PageRect>, arrangement: Arrangement) -> Self {
        let turned: Vec<PageRect> = bounds
            .iter()
            .map(|&b| rotate_rect(b, arrangement.rotation))
            .collect();
        let size = |page: usize| {
            let b = turned[page];
            [f64::from(b[2] - b[0]), f64::from(b[3] - b[1])]
        };
        // Rows: the first page alone, then pairs, when spread.
        let mut rows: Vec<std::ops::Range<usize>> = Vec::new();
        let mut page = 0;
        while page < bounds.len() {
            let end = if arrangement.spread && page > 0 {
                (page + 2).min(bounds.len())
            } else {
                page + 1
            };
            rows.push(page..end);
            page = end;
        }
        let row_width = |row: &std::ops::Range<usize>| {
            row.clone().map(|page| size(page)[0]).sum::<f64>()
                + PAGE_GAP * (row.len().saturating_sub(1)) as f64
        };
        let width = rows.iter().map(row_width).fold(0.0, f64::max);
        let mut offsets = vec![[0.0; 2]; bounds.len()];
        let mut y = 0.0;
        for row in &rows {
            let mut x = (width - row_width(row)) / 2.0;
            let mut height: f64 = 0.0;
            for page in row.clone() {
                offsets[page] = [x, y];
                x += size(page)[0] + PAGE_GAP;
                height = height.max(size(page)[1]);
            }
            y += height + PAGE_GAP;
        }
        Self {
            bounds,
            offsets,
            width,
            height: (y - PAGE_GAP).max(0.0),
            arrangement,
            turned,
        }
    }

    pub fn len(&self) -> usize {
        self.bounds.len()
    }

    /// A page's size as laid out (turned).
    pub fn page_size(&self, page: usize) -> [f64; 2] {
        let b = self.turned[page];
        [f64::from(b[2] - b[0]), f64::from(b[3] - b[1])]
    }

    /// The document position of a page point.
    pub fn to_document(&self, page: usize, point: [f32; 2]) -> [f64; 2] {
        let turned = rotate(point, self.arrangement.rotation);
        let (offset, b) = (self.offsets[page], self.turned[page]);
        [
            offset[0] + f64::from(turned[0] - b[0]),
            offset[1] + f64::from(turned[1] - b[1]),
        ]
    }

    /// The page point at a document position.
    pub fn to_page(&self, page: usize, point: [f64; 2]) -> [f32; 2] {
        let (offset, b) = (self.offsets[page], self.turned[page]);
        let turned = [
            b[0] + (point[0] - offset[0]) as f32,
            b[1] + (point[1] - offset[1]) as f32,
        ];
        rotate(turned, (4 - self.arrangement.rotation % 4) % 4)
    }

    /// The document rectangle covering a page rectangle.
    pub fn rect_to_document(&self, page: usize, rect: PageRect) -> [f64; 4] {
        let [a, b] = [
            self.to_document(page, [rect[0], rect[1]]),
            self.to_document(page, [rect[2], rect[3]]),
        ];
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[0].max(b[0]),
            a[1].max(b[1]),
        ]
    }

    /// The first page of the row after (or before) `page`'s, if any.
    pub fn row_step(&self, page: usize, forward: bool) -> Option<usize> {
        let row = self.offsets[page][1];
        if forward {
            (page + 1..self.len()).find(|&next| self.offsets[next][1] > row)
        } else {
            let before = (0..page)
                .rev()
                .find(|&previous| self.offsets[previous][1] < row)?;
            let start = self.offsets[before][1];
            (0..=before).find(|&first| self.offsets[first][1] == start)
        }
    }

    /// Pages overlapping the document rows `y0..y1`.
    pub fn visible(&self, y0: f64, y1: f64) -> Range<usize> {
        let first = self
            .offsets
            .partition_point(|offset| offset[1] < y0)
            .saturating_sub(1);
        // The row's first page, so a spread's left page is not skipped.
        let row = self.offsets.get(first).map_or(0.0, |offset| offset[1]);
        let first = self.offsets[..first].partition_point(|offset| offset[1] < row);
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
        let b = self.turned[page];
        let offset = self.offsets[page];
        Camera {
            origin: [
                (origin[0] - offset[0] + f64::from(b[0])) as f32,
                (origin[1] - offset[1] + f64::from(b[1])) as f32,
            ],
            scale,
            viewport,
            rotation: self.arrangement.rotation,
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

/// Where a page's drawing and text come from: the document, or a
/// translated preview of the page (a one-page PDF).
#[derive(Clone, Debug)]
pub enum PageSource {
    Original,
    Preview { revision: u64, pdf: Arc<[u8]> },
}

impl PageSource {
    /// 0 for the original, then one per preview revision.
    pub fn version(&self) -> u64 {
        match self {
            Self::Original => 0,
            Self::Preview { revision, .. } => revision + 1,
        }
    }
}

/// A page result from the loader. `version` is the [`PageSource`] it was
/// made from.
pub enum Loaded {
    /// What the loading animation traces of a page, sent before its
    /// thumbnail and vectors.
    Sketch {
        page: usize,
        version: u64,
        sketch: Arc<PageSketch>,
    },
    Thumbnail {
        page: usize,
        image: egui::ColorImage,
    },
    Prepared {
        page: usize,
        version: u64,
        prepared: Box<PreparedPage>,
        layers: Vec<(usize, LayerImage)>,
        links: Vec<PageLink>,
        seconds: f64,
    },
    /// A page's text, read after the page is shown or by the indexer.
    Text {
        page: usize,
        version: u64,
        text: Arc<PageText>,
    },
    /// The document's own text of a page shown from a preview, for showing
    /// the original of translated text.
    OriginalText {
        page: usize,
        text: Arc<PageText>,
    },
    /// A page rendered at `scale` pixels per point for the page overview.
    PageImage {
        page: usize,
        scale: f32,
        image: egui::ColorImage,
    },
    /// The indexer skipped a thumbnail: the document is too dense.
    ThumbnailSkipped {
        page: usize,
    },
    Failed {
        page: usize,
        error: String,
    },
    /// A raster layer re-rasterized for the current view.
    Layer {
        page: usize,
        version: u64,
        layer: usize,
        image: LayerImage,
    },
}

/// Work for the loader thread.
enum Request {
    Page {
        page: usize,
        source: PageSource,
    },
    Layer {
        page: usize,
        source: PageSource,
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
    /// Pages asked for and the version of each.
    requested: HashMap<usize, u64>,
}

/// Opens the document a worker reads, with the tab's layout.
fn open_document(path: &Path, layout: DocumentLayout) -> Result<GlyphstDocument> {
    DocumentSnapshot::read(path)?.open_with_layout(layout)
}

impl Loader {
    pub fn new(path: PathBuf, layout: DocumentLayout, ctx: egui::Context) -> Self {
        let (requests, request_rx) = mpsc::channel::<Request>();
        let (result_tx, results) = mpsc::channel();
        let wanted = Arc::new(Mutex::new(HashSet::new()));
        let worker_wanted = wanted.clone();
        thread::Builder::new()
            .name("page-loader".into())
            .spawn(move || {
                let document = match open_document(&path, layout) {
                    Ok(document) => document,
                    Err(error) => {
                        let _ = result_tx.send(Loaded::Failed {
                            page: 0,
                            error: format!("{error:#}"),
                        });
                        return;
                    }
                };
                // Display lists by page and source version.
                let mut lists: VecDeque<((usize, u64), DisplayList)> = VecDeque::new();
                let mut send = |loaded| {
                    let sent = result_tx.send(loaded).is_ok();
                    ctx.request_repaint();
                    sent
                };
                while let Ok(request) = request_rx.recv() {
                    match request {
                        Request::Page { page, source } => {
                            if !worker_wanted
                                .lock()
                                .is_ok_and(|wanted| wanted.contains(&page))
                            {
                                continue;
                            }
                            let key = (page, source.version());
                            if let Some(list) =
                                load_page(document.inner(), page, &source, &mut send)
                            {
                                lists.retain(|(cached, _)| *cached != key);
                                lists.push_back((key, list));
                                if lists.len() > CACHED_LISTS {
                                    lists.pop_front();
                                }
                            }
                        }
                        Request::Layer {
                            page,
                            source,
                            layer,
                            ops,
                            rect,
                            scale,
                        } => {
                            let version = source.version();
                            let image = rasterize_cached(
                                document.inner(),
                                &mut lists,
                                page,
                                &source,
                                ops,
                                rect,
                                scale,
                            );
                            if let Some(image) = image
                                && !send(Loaded::Layer {
                                    page,
                                    version,
                                    layer,
                                    image,
                                })
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
            requested: HashMap::new(),
        }
    }

    /// Requests `pages` (in priority order) from their `source` and
    /// forgets other pages that were requested but not loaded yet. A page
    /// asked for from another source is requested again.
    pub fn want(
        &mut self,
        pages: &[usize],
        loaded: impl Fn(usize) -> bool,
        source: impl Fn(usize) -> PageSource,
    ) {
        if let Ok(mut wanted) = self.wanted.lock() {
            wanted.clear();
            wanted.extend(pages.iter().copied().filter(|&page| !loaded(page)));
        }
        self.requested.retain(|page, _| pages.contains(page));
        for &page in pages {
            if loaded(page) {
                continue;
            }
            let source = source(page);
            if self.requested.get(&page) != Some(&source.version()) {
                self.requested.insert(page, source.version());
                let _ = self.requests.send(Request::Page { page, source });
            }
        }
    }

    /// Re-rasterizes raster layer `layer` of `page` (its display-list `ops`)
    /// over page area `rect` at `scale` device pixels per point.
    pub fn rasterize_layer(
        &self,
        page: usize,
        source: PageSource,
        layer: usize,
        ops: Range<u32>,
        rect: PageRect,
        scale: f32,
    ) {
        let _ = self.requests.send(Request::Layer {
            page,
            source,
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
    source: &PageSource,
    send: &mut dyn FnMut(Loaded) -> bool,
) -> Option<DisplayList> {
    let started = Instant::now();
    let version = source.version();
    let result = (|| -> Result<Option<(Loaded, DisplayList)>> {
        let loaded_page = source_page(document, page, source)?;
        let list = loaded_page.to_display_list(true)?;
        let links = page_links(&loaded_page);
        let vector = capture_display_list(&list)?;
        let sketch = Arc::new(PageSketch::from_vector(&vector));
        if !send(Loaded::Sketch {
            page,
            version,
            sketch,
        }) {
            return Ok(None);
        }
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
            version,
            prepared: Box::new(prepared),
            layers,
            links,
            seconds: started.elapsed().as_secs_f64(),
        };
        Ok(Some((loaded, list)))
    })();
    match result {
        Ok(Some((loaded, list))) => {
            if !send(loaded) {
                return None;
            }
            // The text follows the page, so a dense page shows without
            // waiting for it; a page whose text cannot be read still shows.
            let text = PageText::from_display_list(&list).unwrap_or_default();
            if !send(Loaded::Text {
                page,
                version,
                text: Arc::new(text),
            }) {
                return None;
            }
            if version != 0 {
                let original = original_text(document, page);
                if !send(Loaded::OriginalText {
                    page,
                    text: Arc::new(original),
                }) {
                    return None;
                }
            }
            Some(list)
        }
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

/// The MuPDF page a source draws: the document's page, or the first page
/// of a preview PDF.
fn source_page(
    document: &mupdf::Document,
    page: usize,
    source: &PageSource,
) -> Result<mupdf::Page> {
    match source {
        PageSource::Original => document
            .load_page(i32::try_from(page)?)
            .with_context(|| format!("failed to load page {}", page + 1)),
        PageSource::Preview { pdf, .. } => mupdf::Document::from_bytes(pdf, "application/pdf")
            .and_then(|preview| preview.load_page(0))
            .with_context(|| format!("failed to open the translated page {}", page + 1)),
    }
}

/// The document's own text of a page; empty when it cannot be read.
fn original_text(document: &mupdf::Document, page: usize) -> PageText {
    i32::try_from(page)
        .ok()
        .and_then(|index| document.load_page(index).ok())
        .and_then(|page| PageText::from_page(&page).ok())
        .unwrap_or_default()
}

/// Rasterizes layer operations from a cached display list, loading the
/// page's list again when it was dropped.
fn rasterize_cached(
    document: &mupdf::Document,
    lists: &mut VecDeque<((usize, u64), DisplayList)>,
    page: usize,
    source: &PageSource,
    ops: Range<u32>,
    rect: PageRect,
    scale: f32,
) -> Option<LayerImage> {
    let key = (page, source.version());
    if !lists.iter().any(|(cached, _)| *cached == key) {
        let list = source_page(document, page, source)
            .ok()?
            .to_display_list(true)
            .ok()?;
        lists.push_back((key, list));
        if lists.len() > CACHED_LISTS {
            lists.pop_front();
        }
    }
    let (_, list) = lists.iter().find(|(cached, _)| *cached == key)?;
    rasterize_layer(list, ops, rect, scale, MAX_LAYER_DIMENSION)
        .ok()
        .flatten()
}

/// What the indexer is asked for.
#[derive(Default)]
struct IndexWork {
    /// Sidebar thumbnails, in priority order.
    thumbnails: Vec<usize>,
    /// Sharp renders for the page overview, and their pixels per point.
    images: Vec<(usize, f32)>,
    /// Pages whose text to read, in order, and where from.
    texts: VecDeque<(usize, PageSource)>,
}

/// Page text for search and reflow anchors, and thumbnails for the pages
/// panel, made on a second worker so they never hold up visible pages.
/// Thumbnails come first; text is read in the order asked.
pub struct Indexer {
    work: Arc<Mutex<IndexWork>>,
    stop: Arc<AtomicBool>,
    worker: thread::Thread,
    results: Receiver<Loaded>,
}

impl Indexer {
    pub fn new(path: PathBuf, layout: DocumentLayout, ctx: egui::Context) -> Self {
        let work = Arc::new(Mutex::new(IndexWork::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (result_tx, results) = mpsc::channel();
        let (worker_work, worker_stop) = (work.clone(), stop.clone());
        let handle = thread::Builder::new()
            .name("page-indexer".into())
            .spawn(move || {
                let Ok(document) = open_document(&path, layout) else {
                    return;
                };
                let document = document.inner();
                let mut heavy = false;
                let send = |loaded| {
                    let sent = result_tx.send(loaded).is_ok();
                    ctx.request_repaint();
                    sent
                };
                while !worker_stop.load(Ordering::Relaxed) {
                    let (thumbnail, image, text) = {
                        let Ok(mut work) = worker_work.lock() else {
                            return;
                        };
                        let thumbnail =
                            (!work.thumbnails.is_empty()).then(|| work.thumbnails.remove(0));
                        // Too slow on dense documents: the overview keeps
                        // their thumbnails, or says they are dense.
                        if heavy {
                            work.images.clear();
                        }
                        let image = match thumbnail {
                            None if !work.images.is_empty() => Some(work.images.remove(0)),
                            _ => None,
                        };
                        let text = match (thumbnail, image) {
                            (None, None) => work.texts.pop_front(),
                            _ => None,
                        };
                        (thumbnail, image, text)
                    };
                    let sent = if let Some((page, scale)) = image {
                        let started = Instant::now();
                        let rendered = page_image(document, page, scale);
                        heavy |= started.elapsed() > HEAVY_THUMBNAIL;
                        match rendered {
                            Some(image) => send(Loaded::PageImage { page, scale, image }),
                            None => true,
                        }
                    } else if let Some(page) = thumbnail {
                        if heavy {
                            send(Loaded::ThumbnailSkipped { page })
                        } else {
                            let started = Instant::now();
                            let image = page_thumbnail(document, page);
                            heavy = started.elapsed() > HEAVY_THUMBNAIL;
                            match image {
                                Some(image) => send(Loaded::Thumbnail { page, image }),
                                None => send(Loaded::ThumbnailSkipped { page }),
                            }
                        }
                    } else if let Some((page, source)) = text {
                        let text = source_page(document, page, &source)
                            .ok()
                            .and_then(|page| PageText::from_page(&page).ok())
                            .unwrap_or_default();
                        send(Loaded::Text {
                            page,
                            version: source.version(),
                            text: Arc::new(text),
                        })
                    } else {
                        thread::park_timeout(Duration::from_millis(500));
                        true
                    };
                    if !sent {
                        return;
                    }
                }
            })
            .expect("failed to start the page indexer");
        Self {
            work,
            stop,
            worker: handle.thread().clone(),
            results,
        }
    }

    /// Replaces the sidebar thumbnails wanted, in priority order.
    pub fn want_thumbnails(&self, pages: Vec<usize>) {
        if let Ok(mut work) = self.work.lock()
            && work.thumbnails != pages
        {
            work.thumbnails = pages;
            self.worker.unpark();
        }
    }

    /// Replaces the overview's sharp renders wanted, in priority order,
    /// with the pixels per point of each.
    pub fn want_images(&self, pages: Vec<(usize, f32)>) {
        if let Ok(mut work) = self.work.lock()
            && work.images != pages
        {
            work.images = pages;
            self.worker.unpark();
        }
    }

    /// Adds pages whose text to read.
    pub fn request_text(&self, pages: Vec<(usize, PageSource)>) {
        if let Ok(mut work) = self.work.lock() {
            work.texts.extend(pages);
            self.worker.unpark();
        }
    }

    /// Forgets text not read yet.
    pub fn cancel_text(&self) {
        if let Ok(mut work) = self.work.lock() {
            work.texts.clear();
        }
    }

    pub fn try_recv(&self) -> Option<Loaded> {
        self.results.try_recv().ok()
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.unpark();
    }
}

fn page_thumbnail(document: &mupdf::Document, page: usize) -> Option<egui::ColorImage> {
    page_image(document, page, THUMBNAIL_SCALE)
}

/// Page `page` rendered upright at `scale` pixels per point.
fn page_image(document: &mupdf::Document, page: usize, scale: f32) -> Option<egui::ColorImage> {
    let pixmap = document
        .load_page(i32::try_from(page).ok()?)
        .ok()?
        .to_pixmap(
            &Matrix::new_scale(scale, scale),
            &Colorspace::device_rgb(),
            false,
            true,
        )
        .ok()?;
    let size = [pixmap.width() as usize, pixmap.height() as usize];
    Some(egui::ColorImage::from_rgb(size, pixmap.samples()))
}

/// One entry of a document's outline, flattened in reading order.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineEntry {
    pub title: String,
    pub page: Option<usize>,
    /// The page-space y the entry brings to the top, when it names one.
    pub y: Option<f32>,
    pub depth: usize,
    /// The index of the entry it is nested in.
    pub parent: Option<usize>,
}

/// What a tab needs before its first frame, read on the calling thread.
pub struct DocumentInfo {
    pub bounds: Vec<PageRect>,
    pub outline: Vec<OutlineEntry>,
    pub title: Option<String>,
    /// The document's metadata (author, subject, …) that is set.
    pub metadata: Vec<(&'static str, String)>,
    /// The layout the document opened with: reflowable documents laid
    /// out as asked, others fixed.
    pub layout: DocumentLayout,
}

impl DocumentInfo {
    pub fn read(path: &Path, layout: DocumentLayout) -> Result<Self> {
        let opened = open_document(path, layout)?;
        let layout = opened.layout();
        let document = opened.inner();
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
            flatten_outline(&entries, 0, None, &mut outline);
        }
        let title = document
            .metadata(mupdf::MetadataName::Title)
            .ok()
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty());
        let metadata = [
            ("Author", mupdf::MetadataName::Author),
            ("Subject", mupdf::MetadataName::Subject),
            ("Keywords", mupdf::MetadataName::Keywords),
            ("Creator", mupdf::MetadataName::Creator),
            ("Producer", mupdf::MetadataName::Producer),
            ("Created", mupdf::MetadataName::CreationDate),
            ("Modified", mupdf::MetadataName::ModDate),
            ("Format", mupdf::MetadataName::Format),
        ]
        .into_iter()
        .filter_map(|(label, name)| {
            let value = document.metadata(name).ok()?.trim().to_string();
            (!value.is_empty()).then_some((label, value))
        })
        .collect();
        Ok(Self {
            bounds,
            outline,
            title,
            metadata,
            layout,
        })
    }
}

fn flatten_outline(
    entries: &[mupdf::Outline],
    depth: usize,
    parent: Option<usize>,
    out: &mut Vec<OutlineEntry>,
) {
    for entry in entries {
        let index = out.len();
        out.push(OutlineEntry {
            title: entry.title.trim().to_string(),
            page: entry
                .dest
                .as_ref()
                .map(|dest| dest.loc.page_number as usize),
            y: entry
                .dest
                .as_ref()
                .and_then(|dest| glyphst_pdf::text::destination_top(&dest.kind)),
            depth,
            parent,
        });
        flatten_outline(&entry.down, depth + 1, Some(index), out);
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

    #[test]
    fn previews_load_in_place_of_the_document_page() {
        let directory =
            std::env::temp_dir().join(format!("glyphst-reader-preview-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("source.pdf");
        std::fs::write(&path, glyphst_pdf::test_pdf::text_pdf(&["Original words"])).unwrap();
        let preview: Arc<[u8]> = glyphst_pdf::test_pdf::text_pdf(&["Translated words"]).into();
        let mut loader = Loader::new(path, DocumentLayout::Fixed, egui::Context::default());
        loader.want(
            &[0],
            |_| false,
            |_| PageSource::Preview {
                revision: 4,
                pdf: preview.clone(),
            },
        );
        let (mut prepared, mut text, mut original) = (None, None, None);
        let started = Instant::now();
        while original.is_none() {
            assert!(started.elapsed() < Duration::from_secs(30), "timed out");
            match loader.try_recv() {
                Some(Loaded::Prepared { version, .. }) => prepared = Some(version),
                Some(Loaded::Text {
                    version, text: t, ..
                }) => text = Some((version, t)),
                Some(Loaded::OriginalText { text: t, .. }) => original = Some(t),
                Some(Loaded::Failed { error, .. }) => panic!("{error}"),
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(prepared, Some(5), "the preview's version");
        let (version, text) = text.unwrap();
        assert_eq!(
            (version, text.plain_text().as_str()),
            (5, "Translated words")
        );
        assert_eq!(original.unwrap().plain_text(), "Original words");
        let _ = std::fs::remove_dir_all(&directory);
    }
}
