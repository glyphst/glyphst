//! One open document: continuous layout, camera, page loading, outline,
//! presentation mode, the reading position, and its text: selection,
//! links, find, highlights and the reflowed layout of EPUBs.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use glyphst_core::{DocumentLayout, SemanticTextAnchor};
use glyphst_library::DocumentPosition;
use glyphst_pdf::{
    text::{LinkTarget, PageLink, PageText},
    vector::{LayerImage, PageRect},
};
use glyphst_pdf_gpu::PreparedPage;

use crate::{
    annotations::Annotations,
    doc_text::DocText,
    document::{
        DocumentInfo, Indexer, Layout, Loaded, Loader, OutlineEntry, PAGE_GAP, PageSource, View,
    },
    find::Find,
    gamepad::GamepadInput,
    gpu::PageDraw,
    text_overlay::Selection,
    translation::DocSession,
};

/// Zoom limits relative to the fit-width scale.
pub const MIN_ZOOM: f64 = 0.1;
pub const MAX_ZOOM: f64 = 64.0;
const STEP_ZOOM: f64 = 1.25;
/// Pages loaded beyond the visible ones, in each direction.
const PREFETCH_PAGES: usize = 2;
/// View pixels per D-pad step.
const STEP_PIXELS: f64 = 120.0;
/// Presentation pages fill this share of the view.
const PRESENT_FILL: f64 = 0.96;
/// Raster layers are re-rasterized once the view has been still this long.
const LAYER_SETTLE: Duration = Duration::from_millis(150);
/// Largest raster layer side, as the loader rasterizes it.
const MAX_LAYER_PIXELS: f64 = 4096.0;
/// Extra area rasterized around the visible part of a layer, as a share of
/// its size, so small pans stay sharp.
const LAYER_MARGIN: f32 = 0.15;
/// Back and forward history kept per tab, as pdfcraft keeps.
const MAX_HISTORY: usize = 200;
/// View pixels kept above a link or bookmark target.
const TARGET_MARGIN: f64 = 24.0;
/// Sidebar thumbnails kept as textures; the farthest from the current page
/// go first.
const MAX_THUMBNAILS: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewAction {
    PreviousPage,
    NextPage,
    FitWidth,
    TogglePresent,
    ZoomIn,
    ZoomOut,
    /// Scrolls by whole steps; positive scrolls down.
    Step(i32),
    GoToPage(usize),
    /// Goes to a page point (a link or a bookmark), remembering where the
    /// view was.
    GoTo {
        page: usize,
        y: Option<f32>,
    },
    Back,
    Forward,
    /// Scrolls a page rectangle into view if it is not (a search match).
    Reveal {
        page: usize,
        rect: PageRect,
    },
}

/// Input gathered for one frame of the view.
#[derive(Default)]
pub struct ViewInput<'a> {
    /// Multiplicative zoom (Ctrl+wheel, pinch) anchored at the pointer.
    pub zoom: f64,
    /// Scroll and drag in view pixels.
    pub scroll: [f64; 2],
    /// Pointer in view pixels, when over the view.
    pub pointer: Option<[f64; 2]>,
    pub actions: Vec<ViewAction>,
    pub gamepad: Option<&'a GamepadInput>,
    pub seconds: f64,
    /// The frame's time; without it raster layers are not refreshed.
    pub now: Option<Instant>,
}

/// A loaded page waiting for the GPU.
pub struct ReadyPage {
    pub page: usize,
    pub prepared: Box<PreparedPage>,
    pub layers: Vec<(usize, LayerImage)>,
}

/// A raster layer and the texture currently on the GPU for it.
struct LayerState {
    ops: Range<u32>,
    bounds: PageRect,
    /// Page area the texture covers and its device pixels per point.
    rect: PageRect,
    scale: f32,
    pending: bool,
}

/// What a tab draws this frame.
pub struct TabFrame {
    pub draws: Vec<PageDraw>,
    /// Pages still loading: their paper rectangle (in view pixels) and
    /// thumbnail, drawn over the GPU image.
    pub placeholders: Vec<([f32; 4], Option<egui::TextureId>)>,
    /// Pages to keep loaded, in priority order.
    pub wanted: Vec<usize>,
    pub animating: bool,
    /// A presentation-mode page turn happened: its direction.
    pub turn: Option<f32>,
}

/// Where the view was, for Back and Forward.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Place {
    origin: [f64; 2],
    present: Option<usize>,
}

pub struct DocumentTab {
    pub id: u64,
    pub path: PathBuf,
    pub title: String,
    /// Fixed, or the reflowed layout of an EPUB.
    pub document_layout: DocumentLayout,
    pub layout: Layout,
    pub view: View,
    /// View size in device pixels from the last frame.
    pub viewport: [f64; 2],
    /// Device pixels per egui point, set by the app each frame.
    pub pixels_per_point: f64,
    fitted: bool,
    restore: Option<DocumentPosition>,
    loader: Loader,
    indexer: Indexer,
    thumbnails: HashMap<usize, egui::TextureHandle>,
    /// Pages the indexer will not make sidebar thumbnails for.
    pub dense_pages: HashSet<usize>,
    pub failed: HashMap<usize, String>,
    pub ready: VecDeque<ReadyPage>,
    /// Re-rasterized layers waiting for the GPU: page, layer, image.
    pub ready_layers: VecDeque<(usize, usize, LayerImage)>,
    layers: HashMap<usize, Vec<LayerState>>,
    /// The view's last origin and scale, and when they last changed.
    last_view: ([f64; 2], f64, Option<Instant>),
    pub outline: Vec<OutlineEntry>,
    pub present: Option<usize>,
    pub load_seconds: Vec<f64>,
    pub text: DocText,
    /// Pages whose text the indexer was asked for.
    text_requested: Vec<bool>,
    links: HashMap<usize, Vec<PageLink>>,
    pub selection: Option<Selection>,
    pub find: Find,
    pub annotations: Annotations,
    back: Vec<Place>,
    forward: Vec<Place>,
    /// A reflowed position waiting for the text it points into, and the
    /// view origin when it was set: moving the view cancels it.
    pending_anchor: Option<(SemanticTextAnchor, f32, [f64; 2])>,
    /// When the view width last stopped matching the reflowed layout.
    pub reflow_mismatch: Option<Instant>,
    /// The document's translation, when one was started.
    pub translation: Option<DocSession>,
    /// The source version of each page's loaded drawing and of its text.
    loaded_version: HashMap<usize, u64>,
    text_version: Vec<u64>,
    /// The document's own text of pages shown translated.
    pub original_text: HashMap<usize, Arc<PageText>>,
}

impl DocumentTab {
    /// Opens `path` with `layout` (used by reflowable documents only).
    pub fn open(
        id: u64,
        path: &Path,
        ctx: &egui::Context,
        restore: Option<DocumentPosition>,
        layout: DocumentLayout,
    ) -> Result<Self> {
        let info = DocumentInfo::read(path, layout)?;
        if info.bounds.is_empty() {
            anyhow::bail!("{} has no pages", path.display());
        }
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let pages = info.bounds.len();
        Ok(Self {
            id,
            path: path.to_path_buf(),
            title: info.title.unwrap_or(file_name),
            document_layout: info.layout,
            layout: Layout::new(info.bounds),
            view: View::new(1.0),
            viewport: [1.0, 1.0],
            pixels_per_point: 1.0,
            fitted: false,
            restore,
            loader: Loader::new(path.to_path_buf(), info.layout, ctx.clone()),
            indexer: Indexer::new(path.to_path_buf(), info.layout, ctx.clone()),
            thumbnails: HashMap::new(),
            dense_pages: HashSet::new(),
            failed: HashMap::new(),
            ready: VecDeque::new(),
            ready_layers: VecDeque::new(),
            layers: HashMap::new(),
            last_view: ([0.0; 2], 0.0, None),
            outline: info.outline,
            present: None,
            load_seconds: Vec::new(),
            text: DocText::new(pages),
            text_requested: vec![false; pages],
            links: HashMap::new(),
            selection: None,
            find: Find::default(),
            annotations: Annotations::default(),
            back: Vec::new(),
            forward: Vec::new(),
            pending_anchor: None,
            reflow_mismatch: None,
            translation: None,
            loaded_version: HashMap::new(),
            text_version: vec![0; pages],
            original_text: HashMap::new(),
        })
    }

    /// Whether the document is laid out by the reader (an EPUB).
    pub fn is_reflowed(&self) -> bool {
        matches!(self.document_layout, DocumentLayout::Reflow(_))
    }

    /// Lays a reflowed document out again, keeping the reading position:
    /// everything made for the old layout is dropped, so the caller drops
    /// the tab's GPU pages too.
    pub fn relayout(&mut self, layout: DocumentLayout, ctx: &egui::Context) -> Result<()> {
        let position = self.position();
        let info = DocumentInfo::read(&self.path, layout)?;
        if info.bounds.is_empty() {
            anyhow::bail!("{} has no pages", self.path.display());
        }
        let pages = info.bounds.len();
        self.document_layout = info.layout;
        self.layout = Layout::new(info.bounds);
        self.outline = info.outline;
        self.loader = Loader::new(self.path.clone(), info.layout, ctx.clone());
        self.indexer = Indexer::new(self.path.clone(), info.layout, ctx.clone());
        self.thumbnails.clear();
        self.dense_pages.clear();
        self.failed.clear();
        self.ready.clear();
        self.ready_layers.clear();
        self.layers.clear();
        self.text = DocText::new(pages);
        self.text_requested = vec![false; pages];
        self.links.clear();
        self.selection = None;
        self.find.restart();
        self.annotations.clear_resolved();
        self.back.clear();
        self.forward.clear();
        self.pending_anchor = None;
        self.reflow_mismatch = None;
        self.translation = None;
        self.loaded_version.clear();
        self.text_version = vec![0; pages];
        self.original_text.clear();
        self.restore = Some(position);
        self.fitted = false;
        Ok(())
    }

    /// Takes the loader's results: thumbnails become textures, prepared
    /// pages wait in `ready` for the GPU.
    pub fn receive(&mut self, ctx: &egui::Context) {
        while let Some(loaded) = self.loader.try_recv().or_else(|| self.indexer.try_recv()) {
            match loaded {
                Loaded::Thumbnail { page, image } => {
                    let texture = ctx.load_texture(
                        format!("thumbnail-{}-{page}", self.id),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.thumbnails.insert(page, texture);
                }
                Loaded::Prepared {
                    page,
                    version,
                    prepared,
                    layers,
                    links,
                    seconds,
                } => {
                    // A drawing from a source no longer shown is dropped.
                    if version != self.source(page).version() {
                        continue;
                    }
                    self.loaded_version.insert(page, version);
                    self.load_seconds.push(seconds);
                    self.links.insert(page, links);
                    let states = prepared
                        .raster_layers
                        .iter()
                        .enumerate()
                        .map(|(index, source)| {
                            let image = layers.iter().find(|(layer, _)| *layer == index);
                            let (rect, scale) = image.map_or(([0.0; 4], 0.0), |(_, image)| {
                                (
                                    image.rect,
                                    image.size[0] as f32
                                        / (image.rect[2] - image.rect[0]).max(f32::EPSILON),
                                )
                            });
                            LayerState {
                                ops: source.ops.clone(),
                                bounds: source.bounds,
                                rect,
                                scale,
                                pending: false,
                            }
                        })
                        .collect();
                    self.layers.insert(page, states);
                    self.ready.push_back(ReadyPage {
                        page,
                        prepared,
                        layers,
                    });
                }
                Loaded::Failed { page, error } => {
                    self.failed.insert(page, error);
                }
                Loaded::Text {
                    page,
                    version,
                    text,
                } => {
                    if version == self.source(page).version() {
                        if self.text_version[page] != version {
                            self.text.remove(page);
                            self.find.invalidate_page(page);
                            self.text_version[page] = version;
                        }
                        self.text.insert(page, text);
                    }
                }
                Loaded::OriginalText { page, text } => {
                    self.original_text.insert(page, text);
                }
                Loaded::ThumbnailSkipped { page } => {
                    self.dense_pages.insert(page);
                }
                Loaded::Layer {
                    page,
                    version,
                    layer,
                    image,
                } => {
                    if self.loaded_version.get(&page) != Some(&version) {
                        continue;
                    }
                    if let Some(state) = self
                        .layers
                        .get_mut(&page)
                        .and_then(|states| states.get_mut(layer))
                    {
                        state.rect = image.rect;
                        state.scale = image.size[0] as f32
                            / (image.rect[2] - image.rect[0]).max(f32::EPSILON);
                        state.pending = false;
                    }
                    self.ready_layers.push_back((page, layer, image));
                }
            }
        }
    }

    /// Asks the loader for `wanted` pages that `resident` says are missing.
    /// A page counts as loaded once its drawing from the source shown is on
    /// the GPU or waiting for it.
    pub fn want(&mut self, wanted: &[usize], resident: impl Fn(usize) -> bool) {
        let waiting: Vec<usize> = self.ready.iter().map(|ready| ready.page).collect();
        let current =
            |page: usize| self.loaded_version.get(&page) == Some(&self.source(page).version());
        let loaded: HashSet<usize> = wanted
            .iter()
            .copied()
            .filter(|&page| (resident(page) || waiting.contains(&page)) && current(page))
            .collect();
        let sources: HashMap<usize, PageSource> = wanted
            .iter()
            .map(|&page| (page, self.source(page)))
            .collect();
        self.loader.want(
            wanted,
            |page| loaded.contains(&page),
            |page| sources.get(&page).cloned().unwrap_or(PageSource::Original),
        );
    }

    /// Where `page` is drawn from: its translation when one is shown,
    /// otherwise the document.
    pub fn source(&self, page: usize) -> PageSource {
        match self
            .translation
            .as_ref()
            .and_then(|session| session.shown_preview(page))
        {
            Some(preview) => PageSource::Preview {
                revision: preview.revision,
                pdf: preview.pdf.clone(),
            },
            None => PageSource::Original,
        }
    }

    /// The translated block under a page point, and the original text it
    /// replaces (read from the document's text in the block's source
    /// regions).
    pub fn original_at(&self, page: usize, point: [f32; 2]) -> Option<(PageRect, String)> {
        let preview = self.translation.as_ref()?.shown_preview(page)?;
        let inside = |b: &glyphst_pdf_translation_service::BoundingBox| {
            point[0] >= b.x0 && point[0] <= b.x1 && point[1] >= b.y0 && point[1] <= b.y1
        };
        let region = preview
            .hover
            .iter()
            .filter(|region| inside(&region.target_bounds))
            .min_by(|a, b| {
                let area = |r: &glyphst_pdf_translation_service::BoundingBox| {
                    (r.x1 - r.x0) * (r.y1 - r.y0)
                };
                area(&a.target_bounds).total_cmp(&area(&b.target_bounds))
            })?;
        let original = self.original_text.get(&page)?;
        let glyphs: Vec<usize> = region
            .source_regions
            .iter()
            .flat_map(|b| original.glyphs_in([b.x0, b.y0, b.x1, b.y1]))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let text = original.text_of_glyphs(&glyphs);
        let b = &region.target_bounds;
        (!text.is_empty()).then_some(([b.x0, b.y0, b.x1, b.y1], text))
    }

    /// Whether `page` shows its translation.
    pub fn shows_translation(&self, page: usize) -> bool {
        self.source(page).version() != 0
    }

    /// Follows the translation: pages whose shown source changed (a new
    /// translated page, or the view switched between original and
    /// translation) are drawn and read again.
    pub fn sync_translation(&mut self) {
        if let Some(session) = &mut self.translation {
            session.poll();
        }
        for page in 0..self.layout.len() {
            let version = self.source(page).version();
            if self.text_version[page] != version {
                self.text.remove(page);
                self.find.invalidate_page(page);
                self.text_version[page] = version;
                self.text_requested[page] = false;
                self.thumbnails.remove(&page);
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.page == page)
                {
                    self.selection = None;
                }
            }
        }
    }

    /// Opens at `zoom` (relative to fit width) instead of the remembered
    /// zoom; for scripted runs.
    pub fn set_initial_zoom(&mut self, zoom: f32) {
        let mut position = self
            .restore
            .take()
            .unwrap_or_else(|| DocumentPosition::fixed(0.0, 0.0, 1.0, 0));
        position.zoom = zoom;
        self.restore = Some(position);
    }

    /// Whether raster layers are being re-rasterized or wait for the GPU.
    pub fn layers_pending(&self) -> bool {
        !self.ready_layers.is_empty() || self.layers.values().flatten().any(|state| state.pending)
    }

    /// Lets a page dropped from the GPU be loaded again.
    pub fn forget(&mut self, page: usize) {
        self.loader.forget(page);
        self.layers.remove(&page);
        self.links.remove(&page);
        self.loaded_version.remove(&page);
        self.original_text.remove(&page);
        // Text goes with the page unless a search, the reflowed layout or
        // the selection needs it.
        let selected = self.selection.as_ref().map(|selection| selection.page);
        if !self.find.is_active() && !self.is_reflowed() && selected != Some(page) {
            self.text.remove(page);
        }
    }

    /// Keeps the text the tab needs: every page while searching and for
    /// reflowed documents (their positions and highlights are found by
    /// text), otherwise only pages near the view and the selection's.
    /// Runs the search over text as it arrives.
    pub fn update_text(&mut self, now: Instant) {
        let current = self.current_page();
        let searching = self.find.refresh(&self.text, now, current);
        let everything = searching || self.find.is_active() || self.is_reflowed();
        if everything {
            let missing: Vec<usize> = (0..self.layout.len())
                .filter(|&page| self.text.get(page).is_none() && !self.text_requested[page])
                .collect();
            if !missing.is_empty() {
                for &page in &missing {
                    self.text_requested[page] = true;
                }
                self.indexer.request_text(
                    missing
                        .into_iter()
                        .map(|page| (page, self.source(page)))
                        .collect(),
                );
            }
        } else if self.text_requested.iter().any(|requested| *requested) {
            self.indexer.cancel_text();
            self.text_requested.fill(false);
            let near = |page: usize| page.abs_diff(current) <= 8;
            let selected = self.selection.as_ref().map(|selection| selection.page);
            self.text.retain(|page| {
                near(page) || Some(page) == selected || self.links.contains_key(&page)
            });
        }
        if self.is_reflowed() {
            self.annotations.resolve(&self.text);
            self.resolve_pending_anchor();
        }
    }

    /// Asks for sidebar thumbnails of `pages` (the rows shown), dropping
    /// textures far from them when there are many.
    pub fn want_thumbnails(&mut self, pages: Range<usize>) {
        let missing: Vec<usize> = pages
            .clone()
            .filter(|page| !self.thumbnails.contains_key(page) && !self.dense_pages.contains(page))
            .collect();
        self.indexer.want_thumbnails(missing);
        if self.thumbnails.len() > MAX_THUMBNAILS {
            let center = (pages.start + pages.end) / 2;
            let mut far: Vec<usize> = self
                .thumbnails
                .keys()
                .copied()
                .filter(|page| !pages.contains(page))
                .collect();
            far.sort_by_key(|page| std::cmp::Reverse(page.abs_diff(center)));
            for page in far.into_iter().take(self.thumbnails.len() - MAX_THUMBNAILS) {
                self.thumbnails.remove(&page);
            }
        }
    }

    pub fn thumbnail(&self, page: usize) -> Option<&egui::TextureHandle> {
        self.thumbnails.get(&page)
    }

    /// The link under a page point.
    pub fn link_at(&self, page: usize, point: [f32; 2]) -> Option<&LinkTarget> {
        self.links.get(&page)?.iter().rev().find_map(|link| {
            let r = link.rect;
            (point[0] >= r[0] && point[0] <= r[2] && point[1] >= r[1] && point[1] <= r[3])
                .then_some(&link.target)
        })
    }

    /// The text of the selection.
    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection.as_ref()?;
        let text = selection.text(self.text.get(selection.page)?);
        (!text.is_empty()).then_some(text)
    }

    /// Selects all text of the page at the view's center.
    pub fn select_page(&mut self, page: usize) {
        if let Some(text) = self.text.get(page)
            && !text.is_empty()
        {
            self.selection = Some(Selection::run(page, 0..text.len()));
        }
    }

    fn place(&self) -> Place {
        Place {
            origin: self.view.origin,
            present: self.present,
        }
    }

    fn go_to_place(&mut self, place: Place) {
        self.view.origin = place.origin;
        if self.present.is_some() {
            self.present = place.present.or(self.present);
        }
    }

    /// Remembers the view before a jump.
    fn remember(&mut self) {
        let place = self.place();
        if self.back.last() != Some(&place) {
            self.back.push(place);
            if self.back.len() > MAX_HISTORY {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Document y of a page point.
    fn document_y(&self, page: usize, y: f32) -> f64 {
        self.layout.offsets[page][1] + f64::from(y - self.layout.bounds[page][1])
    }

    /// Shows page point `y` of `page` near the top of the view.
    fn go_to_point(&mut self, page: usize, y: Option<f32>) {
        let page = page.min(self.layout.len() - 1);
        self.go_to_page(page);
        if let Some(y) = y
            && self.present.is_none()
        {
            let y = self.document_y(
                page,
                y.clamp(self.layout.bounds[page][1], self.layout.bounds[page][3]),
            );
            self.view.origin[1] = y - TARGET_MARGIN / self.view.scale;
        }
    }

    /// Goes to a saved position (a bookmark) without changing the zoom.
    pub fn go_to_position(&mut self, position: &DocumentPosition) {
        self.remember();
        let page = position.page.saturating_sub(1).min(self.layout.len() - 1);
        let into = f64::from(position.scroll_y_doc) - self.layout.offsets[page][1];
        let y = (0.0..self.layout.page_size(page)[1])
            .contains(&into)
            .then(|| self.layout.bounds[page][1] + into as f32);
        self.go_to_point(page, y.filter(|_| position.semantic.is_none()));
        if self.is_reflowed()
            && let Some(semantic) = &position.semantic
        {
            self.view.clamp(&self.layout, self.viewport);
            self.pending_anchor = Some((semantic.anchor.clone(), 0.0, self.view.origin));
            self.resolve_pending_anchor();
        }
    }

    /// The first words at the top of the view, to name a bookmark.
    pub fn words_at_top(&self, count: usize) -> Option<String> {
        let page = self.position().page.saturating_sub(1);
        let text = self.text.get(page)?;
        let top = (self.view.origin[1] - self.layout.offsets[page][1]) as f32
            + self.layout.bounds[page][1];
        let first = text.glyphs.iter().position(|glyph| glyph.rect[3] >= top)?;
        let words: Vec<String> = text
            .text_of(first..(first + 200).min(text.len()))
            .split_whitespace()
            .take(count)
            .map(str::to_string)
            .collect();
        (!words.is_empty()).then(|| words.join(" "))
    }

    /// Scrolls so a page rectangle is in view, centring it when it is not.
    fn reveal(&mut self, page: usize, rect: PageRect) {
        let page = page.min(self.layout.len() - 1);
        if self.present.is_some() {
            self.present = Some(page);
            return;
        }
        let offset = self.layout.offsets[page];
        let b = self.layout.bounds[page];
        let x0 = offset[0] + f64::from(rect[0] - b[0]);
        let x1 = offset[0] + f64::from(rect[2] - b[0]);
        let y0 = self.document_y(page, rect[1]);
        let y1 = self.document_y(page, rect[3]);
        let [width, height] = [
            self.viewport[0] / self.view.scale,
            self.viewport[1] / self.view.scale,
        ];
        let margin = TARGET_MARGIN / self.view.scale;
        let origin = self.view.origin;
        if y0 < origin[1] + margin || y1 > origin[1] + height - margin {
            self.view.origin[1] = (y0 + y1) / 2.0 - height / 2.0;
        }
        if x0 < origin[0] || x1 > origin[0] + width {
            self.view.origin[0] = (x0 + x1) / 2.0 - width / 2.0;
        }
    }

    /// Once the view has been still for [`LAYER_SETTLE`], asks for sharper
    /// textures of visible raster layers whose texture is too coarse or
    /// does not cover what is shown.
    fn refresh_layers(&mut self, draws: &[PageDraw], now: Instant) {
        let view = (self.view.origin, self.view.scale);
        if (view.0, view.1) != (self.last_view.0, self.last_view.1) || self.last_view.2.is_none() {
            self.last_view = (view.0, view.1, Some(now));
            return;
        }
        if self
            .last_view
            .2
            .is_some_and(|changed| now - changed < LAYER_SETTLE)
        {
            return;
        }
        for draw in draws {
            let Some(states) = self.layers.get_mut(&draw.page) else {
                continue;
            };
            let camera = draw.camera;
            let shown = [
                camera.origin[0],
                camera.origin[1],
                camera.origin[0] + camera.viewport[0] as f32 / camera.scale,
                camera.origin[1] + camera.viewport[1] as f32 / camera.scale,
            ];
            for (index, state) in states.iter_mut().enumerate() {
                if state.pending {
                    continue;
                }
                let Some(visible) = intersect(shown, state.bounds) else {
                    continue;
                };
                let margin = [visible[2] - visible[0], visible[3] - visible[1]]
                    .map(|side| side * LAYER_MARGIN);
                let wanted = intersect(
                    [
                        visible[0] - margin[0],
                        visible[1] - margin[1],
                        visible[2] + margin[0],
                        visible[3] + margin[1],
                    ],
                    state.bounds,
                )
                .unwrap_or(visible);
                let needed = f64::from(camera.scale)
                    .min(MAX_LAYER_PIXELS / f64::from(wanted[2] - wanted[0]))
                    .min(MAX_LAYER_PIXELS / f64::from(wanted[3] - wanted[1]))
                    as f32;
                let covers = contains(state.rect, visible);
                if covers && state.scale >= needed * 0.95 {
                    continue;
                }
                state.pending = true;
                let source = match &self.translation {
                    Some(session) => match session.shown_preview(draw.page) {
                        Some(preview) => PageSource::Preview {
                            revision: preview.revision,
                            pdf: preview.pdf.clone(),
                        },
                        None => PageSource::Original,
                    },
                    None => PageSource::Original,
                };
                self.loader.rasterize_layer(
                    draw.page,
                    source,
                    index,
                    state.ops.clone(),
                    wanted,
                    needed,
                );
            }
        }
    }

    /// Device pixels per point that fit the widest page across the view.
    /// A reflowed column is laid out for the view, so it shows at its
    /// actual size (one page point per egui point) when it fits.
    pub fn fit_scale(&self) -> f64 {
        let fit = self.viewport[0] / (self.layout.width + 2.0 * PAGE_GAP);
        if self.is_reflowed() {
            fit.min(self.pixels_per_point)
        } else {
            fit
        }
    }

    fn fit_width(&mut self) {
        let page = self.current_page();
        self.view = View::new(self.fit_scale());
        self.go_to_page(page);
    }

    /// The page at the vertical center of the view.
    pub fn current_page(&self) -> usize {
        if let Some(page) = self.present {
            return page;
        }
        let center = self.view.origin[1] + self.viewport[1] / 2.0 / self.view.scale;
        self.layout
            .offsets
            .partition_point(|offset| offset[1] <= center)
            .saturating_sub(1)
    }

    pub fn go_to_page(&mut self, page: usize) {
        let page = page.min(self.layout.len() - 1);
        if self.present.is_some() {
            self.present = Some(page);
        }
        self.view.origin[0] = -PAGE_GAP;
        self.view.origin[1] = self.layout.offsets[page][1] - PAGE_GAP;
    }

    /// The reading position: the page at the top of the view (numbered
    /// from 1, as the library stores it), the offset into it, and the zoom
    /// relative to fit width. Reflowed documents also record the text at
    /// the top of the view, which outlives a change of layout.
    pub fn position(&self) -> DocumentPosition {
        let top = self
            .layout
            .offsets
            .partition_point(|offset| offset[1] <= self.view.origin[1] + PAGE_GAP)
            .saturating_sub(1);
        let page = self.present.unwrap_or(top);
        let zoom = (self.view.scale / self.fit_scale()) as f32;
        if self.is_reflowed()
            && let Some((anchor, fraction)) = self.anchor_at_top(page)
        {
            let progress = (self.view.origin[1] / self.layout.height.max(1.0)).clamp(0.0, 1.0);
            return DocumentPosition::reflow(anchor, fraction, progress as f32, zoom, page + 1);
        }
        DocumentPosition::fixed(
            self.view.origin[0] as f32,
            self.view.origin[1] as f32,
            zoom,
            page + 1,
        )
    }

    /// The anchor of the first glyph at or below the top of the view on
    /// `page`, and where it is in the view (0 at the top, 1 at the bottom).
    fn anchor_at_top(&self, page: usize) -> Option<(SemanticTextAnchor, f32)> {
        let text = self.text.get(page)?;
        let top = (self.view.origin[1] - self.layout.offsets[page][1]) as f32
            + self.layout.bounds[page][1];
        let glyph = text
            .glyphs
            .iter()
            .position(|glyph| glyph.rect[3] >= top && !glyph.ch.is_whitespace())?;
        let anchor = self.text.anchor(page, glyph)?;
        let y = self.document_y(page, text.glyphs[glyph].rect[1]);
        let height = self.viewport[1] / self.view.scale;
        let fraction = ((y - self.view.origin[1]) / height).clamp(0.0, 1.0) as f32;
        Some((anchor, fraction))
    }

    /// Moves to a reflowed position once its text is known, unless the view
    /// moved meanwhile.
    fn resolve_pending_anchor(&mut self) {
        let Some((anchor, fraction, origin)) = &self.pending_anchor else {
            return;
        };
        if *origin != self.view.origin {
            self.pending_anchor = None;
            return;
        }
        let Some((page, glyph)) = self.text.resolve(anchor) else {
            return;
        };
        let fraction = f64::from(*fraction);
        if let Some(rect) = self.text.get(page).map(|text| text.glyphs[glyph].rect) {
            let height = self.viewport[1] / self.view.scale;
            self.view.origin[1] = self.document_y(page, rect[1]) - fraction * height;
            self.view.clamp(&self.layout, self.viewport);
        }
        self.pending_anchor = None;
    }

    fn apply_restore(&mut self, position: &DocumentPosition) {
        let page = position.page.saturating_sub(1).min(self.layout.len() - 1);
        let zoom = f64::from(position.zoom);
        if zoom.is_finite() && zoom > 0.0 {
            self.view = View::new(self.fit_scale() * zoom.clamp(MIN_ZOOM, MAX_ZOOM));
        }
        self.go_to_page(page);
        // The saved offset into the page, when it is one (positions written
        // by other layouts only place the page).
        let into_page = f64::from(position.scroll_y_doc) - self.layout.offsets[page][1];
        if position.semantic.is_none() && (0.0..self.layout.page_size(page)[1]).contains(&into_page)
        {
            self.view.origin[1] = self.layout.offsets[page][1] + into_page;
        }
        if self.is_reflowed()
            && let Some(semantic) = &position.semantic
        {
            self.view.clamp(&self.layout, self.viewport);
            self.pending_anchor = Some((
                semantic.anchor.clone(),
                semantic.viewport_fraction,
                self.view.origin,
            ));
        }
    }

    fn zoom_limits(&self) -> (f64, f64) {
        let fit = self.fit_scale();
        (fit * MIN_ZOOM, fit * MAX_ZOOM)
    }

    /// Applies an action; returns the direction of a presentation page turn.
    pub fn apply(&mut self, action: ViewAction, turning: bool) -> Option<f32> {
        let center = [self.viewport[0] / 2.0, self.viewport[1] / 2.0];
        let (min, max) = self.zoom_limits();
        let last = self.layout.len() - 1;
        match action {
            ViewAction::PreviousPage | ViewAction::NextPage => {
                let delta = if action == ViewAction::NextPage {
                    1
                } else {
                    -1
                };
                match self.present {
                    Some(current) => {
                        let target = current.saturating_add_signed(delta).min(last);
                        if target != current && !turning {
                            self.present = Some(target);
                            return Some(delta as f32);
                        }
                    }
                    None => {
                        let target = self.current_page().saturating_add_signed(delta).min(last);
                        self.go_to_page(target);
                    }
                }
            }
            ViewAction::FitWidth => self.fit_width(),
            ViewAction::TogglePresent => {
                self.present = match self.present {
                    Some(page) => {
                        self.go_to_page(page);
                        None
                    }
                    None => Some(self.current_page()),
                };
            }
            ViewAction::ZoomIn => self.view.zoom_by(STEP_ZOOM, center, min, max),
            ViewAction::ZoomOut => self.view.zoom_by(1.0 / STEP_ZOOM, center, min, max),
            ViewAction::Step(steps) => self
                .view
                .scroll_by_pixels(0.0, f64::from(steps) * STEP_PIXELS),
            ViewAction::GoToPage(page) => {
                self.remember();
                self.go_to_page(page);
            }
            ViewAction::GoTo { page, y } => {
                self.remember();
                self.go_to_point(page, y);
            }
            ViewAction::Back => {
                if let Some(place) = self.back.pop() {
                    self.forward.push(self.place());
                    self.go_to_place(place);
                }
            }
            ViewAction::Forward => {
                if let Some(place) = self.forward.pop() {
                    self.back.push(self.place());
                    self.go_to_place(place);
                }
            }
            ViewAction::Reveal { page, rect } => self.reveal(page, rect),
        }
        None
    }

    /// Advances the view for this frame and lists what to draw.
    pub fn frame(&mut self, viewport: [u32; 2], input: ViewInput<'_>, turning: bool) -> TabFrame {
        // A view at fit width stays at fit width when it narrows or widens
        // (a side panel opening).
        let at_fit = self.fitted
            && self.view.target_scale == self.view.scale
            && (self.view.scale / self.fit_scale() - 1.0).abs() < 1e-3;
        let width = f64::from(viewport[0]);
        let resized = width != self.viewport[0];
        self.viewport = [width, f64::from(viewport[1])];
        if at_fit && resized {
            let fit = self.fit_scale();
            self.view.anchor = [0.0; 2];
            self.view.set_scale(fit);
            self.view.target_scale = fit;
        }
        if !self.fitted {
            self.view = View::new(self.fit_scale());
            self.go_to_page(0);
            if let Some(position) = self.restore.take() {
                self.apply_restore(&position);
            }
            self.fitted = true;
        }
        let mut turn = None;
        for action in input.actions {
            turn = turn.or(self.apply(action, turning));
        }
        if self.present.is_none() {
            let (min, max) = self.zoom_limits();
            if let Some(pointer) = input.pointer
                && input.zoom != 1.0
                && input.zoom > 0.0
            {
                self.view.zoom_by(input.zoom, pointer, min, max);
            }
            self.view.scroll_by_pixels(input.scroll[0], input.scroll[1]);
            if let Some(pad) = input.gamepad {
                let seconds = input.seconds;
                self.view.scroll_by_pixels(
                    f64::from(pad.scroll[0]) * seconds,
                    f64::from(pad.scroll[1]) * seconds,
                );
                if pad.zoom != 0.0 {
                    let center = [self.viewport[0] / 2.0, self.viewport[1] / 2.0];
                    self.view
                        .zoom_by((f64::from(pad.zoom) * seconds).exp(), center, min, max);
                }
            }
        }
        let animating = self.view.animate(input.seconds);
        self.view.clamp(&self.layout, self.viewport);

        let (draws, wanted) = self.draws_and_wanted(viewport);
        if let Some(now) = input.now {
            self.refresh_layers(&draws, now);
        }
        TabFrame {
            draws,
            placeholders: Vec::new(),
            wanted,
            animating,
            turn,
        }
    }

    /// The pages the view shows, and the pages to keep loaded in priority
    /// order.
    fn draws_and_wanted(&self, viewport: [u32; 2]) -> (Vec<PageDraw>, Vec<usize>) {
        match self.present {
            Some(page) => {
                let last = self.layout.len() - 1;
                let draw = self.presented(page, viewport);
                let wanted = vec![page, (page + 1).min(last), page.saturating_sub(1)];
                (vec![draw], wanted)
            }
            None => {
                let y1 = self.view.origin[1] + self.viewport[1] / self.view.scale;
                let visible = self.layout.visible(self.view.origin[1], y1);
                let draws = visible
                    .clone()
                    .map(|page| self.page_draw(page, &self.view, viewport))
                    .collect();
                let mut wanted: Vec<usize> = visible.clone().collect();
                for distance in 1..=PREFETCH_PAGES {
                    wanted.push(visible.end + distance - 1);
                    if let Some(before) = visible.start.checked_sub(distance) {
                        wanted.push(before);
                    }
                }
                wanted.retain(|&page| page < self.layout.len());
                (draws, wanted)
            }
        }
    }

    /// The pages the view showed last frame, for hit-testing this frame's
    /// pointer before the view moves.
    pub fn visible_draws(&self, viewport: [u32; 2]) -> Vec<PageDraw> {
        if !self.fitted {
            return Vec::new();
        }
        self.draws_and_wanted(viewport).0
    }

    /// Placeholders for the pages of `frame` that `resident` says are not
    /// on the GPU yet.
    pub fn placeholders(&self, frame: &mut TabFrame, resident: impl Fn(usize) -> bool) {
        frame.placeholders = frame
            .draws
            .iter()
            .filter(|draw| !resident(draw.page))
            .map(|draw| (draw.paper, self.thumbnails.get(&draw.page).map(|t| t.id())))
            .collect();
    }

    fn presented(&self, page: usize, viewport: [u32; 2]) -> PageDraw {
        let [width, height] = self.layout.page_size(page);
        let size = [f64::from(viewport[0]), f64::from(viewport[1])];
        let scale = (size[0] / width).min(size[1] / height) * PRESENT_FILL;
        let mut view = View::new(scale);
        view.origin = [
            self.layout.offsets[page][0] + (width - size[0] / scale) / 2.0,
            self.layout.offsets[page][1] + (height - size[1] / scale) / 2.0,
        ];
        self.page_draw(page, &view, viewport)
    }

    fn page_draw(&self, page: usize, view: &View, viewport: [u32; 2]) -> PageDraw {
        let offset = self.layout.offsets[page];
        let [width, height] = self.layout.page_size(page);
        let x0 = (offset[0] - view.origin[0]) * view.scale;
        let y0 = (offset[1] - view.origin[1]) * view.scale;
        PageDraw {
            page,
            camera: self
                .layout
                .page_camera(page, view.origin, view.scale as f32, viewport),
            paper: [
                x0 as f32,
                y0 as f32,
                (x0 + width * view.scale) as f32,
                (y0 + height * view.scale) as f32,
            ],
        }
    }
}

fn intersect(a: PageRect, b: PageRect) -> Option<PageRect> {
    let rect = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    (rect[2] > rect[0] && rect[3] > rect[1]).then_some(rect)
}

fn contains(outer: PageRect, inner: PageRect) -> bool {
    outer[0] <= inner[0] && outer[1] <= inner[1] && outer[2] >= inner[2] && outer[3] >= inner[3]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tab without a document behind it: layout and view logic only.
    fn tab(pages: usize) -> DocumentTab {
        let ctx = egui::Context::default();
        let path = PathBuf::from("/nonexistent.pdf");
        DocumentTab {
            id: 1,
            path: path.clone(),
            title: "t".into(),
            document_layout: DocumentLayout::Fixed,
            layout: Layout::new(vec![[0.0, 0.0, 600.0, 800.0]; pages]),
            view: View::new(1.0),
            viewport: [1.0, 1.0],
            pixels_per_point: 1.0,
            fitted: false,
            restore: None,
            loader: Loader::new(path.clone(), DocumentLayout::Fixed, ctx.clone()),
            indexer: Indexer::new(path, DocumentLayout::Fixed, ctx),
            thumbnails: HashMap::new(),
            dense_pages: HashSet::new(),
            failed: HashMap::new(),
            ready: VecDeque::new(),
            ready_layers: VecDeque::new(),
            layers: HashMap::new(),
            last_view: ([0.0; 2], 0.0, None),
            outline: Vec::new(),
            present: None,
            load_seconds: Vec::new(),
            text: DocText::new(pages),
            text_requested: vec![false; pages],
            links: HashMap::new(),
            selection: None,
            find: Find::default(),
            annotations: Annotations::default(),
            back: Vec::new(),
            forward: Vec::new(),
            pending_anchor: None,
            reflow_mismatch: None,
            translation: None,
            loaded_version: HashMap::new(),
            text_version: vec![0; pages],
            original_text: HashMap::new(),
        }
    }

    fn settle(tab: &mut DocumentTab) -> TabFrame {
        let mut frame = tab.frame(
            [1248, 900],
            ViewInput {
                zoom: 1.0,
                seconds: 1.0,
                ..Default::default()
            },
            false,
        );
        for _ in 0..200 {
            frame = tab.frame(
                [1248, 900],
                ViewInput {
                    zoom: 1.0,
                    seconds: 1.0 / 60.0,
                    ..Default::default()
                },
                false,
            );
        }
        frame
    }

    #[test]
    fn positions_round_trip_through_page_offset_and_zoom() {
        let mut first = tab(20);
        settle(&mut first);
        first.apply(ViewAction::GoToPage(7), false);
        first.view.origin[1] += 300.0;
        first.view.set_scale(first.fit_scale() * 2.0);
        first.view.target_scale = first.view.scale;
        let position = first.position();
        assert_eq!(position.page, 8, "the library numbers pages from 1");

        let mut second = tab(20);
        second.restore = Some(position.clone());
        settle(&mut second);
        assert_eq!(second.position().page, 8);
        assert!((second.view.origin[1] - first.view.origin[1]).abs() < 1e-3);
        assert!((second.view.scale / second.fit_scale() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn positions_from_another_layout_still_open_the_page() {
        let mut tab = tab(20);
        tab.restore = Some(DocumentPosition::fixed(0.0, 99_999.0, 1.0, 5));
        settle(&mut tab);
        assert_eq!(tab.position().page, 5);
    }

    #[test]
    fn presentation_turns_report_their_direction_once() {
        let mut tab = tab(3);
        settle(&mut tab);
        tab.apply(ViewAction::TogglePresent, false);
        assert_eq!(tab.apply(ViewAction::NextPage, false), Some(1.0));
        assert_eq!(tab.present, Some(1));
        assert_eq!(
            tab.apply(ViewAction::NextPage, true),
            None,
            "no turn while turning"
        );
        assert_eq!(tab.apply(ViewAction::PreviousPage, false), Some(-1.0));
        assert_eq!(
            tab.apply(ViewAction::PreviousPage, false),
            None,
            "at the first page"
        );
        let frame = tab.frame(
            [1248, 900],
            ViewInput {
                zoom: 1.0,
                seconds: 0.0,
                ..Default::default()
            },
            false,
        );
        assert_eq!(frame.draws.len(), 1);
        assert_eq!(frame.wanted[0], 0);
    }

    #[test]
    fn continuous_frames_draw_visible_pages_and_prefetch_neighbours() {
        let mut tab = tab(20);
        let frame = settle(&mut tab);
        assert_eq!(frame.draws[0].page, 0);
        let visible = frame.draws.len();
        assert!(frame.wanted.len() > visible, "neighbours are prefetched");
        assert!(frame.wanted.iter().all(|&page| page < 20));
    }

    #[test]
    fn coarse_layers_are_refreshed_only_after_the_view_settles() {
        let mut tab = tab(3);
        settle(&mut tab);
        tab.layers.insert(
            0,
            vec![LayerState {
                ops: 0..1,
                bounds: [0.0, 0.0, 600.0, 800.0],
                rect: [0.0, 0.0, 600.0, 800.0],
                scale: 0.5,
                pending: false,
            }],
        );
        let start = Instant::now();
        let frame = |tab: &mut DocumentTab, now| {
            tab.frame(
                [1248, 900],
                ViewInput {
                    zoom: 1.0,
                    now: Some(now),
                    ..Default::default()
                },
                false,
            )
        };
        frame(&mut tab, start);
        frame(&mut tab, start + Duration::from_millis(50));
        assert!(!tab.layers[&0][0].pending, "still settling");
        frame(&mut tab, start + Duration::from_millis(200));
        assert!(
            tab.layers[&0][0].pending,
            "the 0.5 px/pt texture is too coarse"
        );

        // A texture that covers the view at its scale is left alone.
        let scale = tab.view.scale as f32;
        let state = &mut tab.layers.get_mut(&0).unwrap()[0];
        state.pending = false;
        state.scale = scale;
        frame(&mut tab, start + Duration::from_millis(400));
        assert!(!tab.layers[&0][0].pending);
    }

    #[test]
    fn links_jump_to_their_point_and_back_and_forward_return() {
        let mut tab = tab(20);
        settle(&mut tab);
        tab.apply(ViewAction::GoToPage(3), false);
        let start = tab.view.origin;
        // A link to page 9, 200 pt down the page.
        tab.apply(
            ViewAction::GoTo {
                page: 9,
                y: Some(200.0),
            },
            false,
        );
        let shown = tab.view.origin[1] + TARGET_MARGIN / tab.view.scale;
        assert!((shown - (tab.layout.offsets[9][1] + 200.0)).abs() < 1e-6);
        assert!(tab.can_go_back() && !tab.can_go_forward());
        tab.apply(ViewAction::Back, false);
        assert_eq!(tab.view.origin, start);
        tab.apply(ViewAction::Back, false);
        assert_eq!(tab.current_page(), 0, "back to before the first jump");
        assert!(!tab.can_go_back());
        tab.apply(ViewAction::Forward, false);
        tab.apply(ViewAction::Forward, false);
        assert_eq!(tab.position().page, 10);
        // A new jump forgets the forward history.
        tab.apply(ViewAction::Back, false);
        tab.apply(ViewAction::GoToPage(1), false);
        assert!(!tab.can_go_forward());
    }

    #[test]
    fn revealing_a_match_scrolls_only_when_it_is_out_of_view() {
        let mut tab = tab(20);
        settle(&mut tab);
        let origin = tab.view.origin;
        tab.apply(
            ViewAction::Reveal {
                page: 0,
                rect: [100.0, 100.0, 140.0, 112.0],
            },
            false,
        );
        assert_eq!(tab.view.origin, origin, "already visible");
        tab.apply(
            ViewAction::Reveal {
                page: 12,
                rect: [100.0, 400.0, 140.0, 412.0],
            },
            false,
        );
        let center = tab.view.origin[1] + tab.viewport[1] / tab.view.scale / 2.0;
        assert!((center - (tab.layout.offsets[12][1] + 406.0)).abs() < 1e-6);
        assert!(
            !tab.can_go_back(),
            "stepping through matches keeps no history"
        );
    }

    #[test]
    fn a_view_at_fit_width_follows_the_view_width() {
        let mut tab = tab(5);
        settle(&mut tab);
        let frame = |tab: &mut DocumentTab, width| {
            tab.frame(
                [width, 900],
                ViewInput {
                    zoom: 1.0,
                    ..Default::default()
                },
                false,
            )
        };
        frame(&mut tab, 900);
        assert!((tab.view.scale / tab.fit_scale() - 1.0).abs() < 1e-9);
        // Zoomed in, a narrower view keeps the zoom.
        tab.view.set_scale(tab.fit_scale() * 2.0);
        tab.view.target_scale = tab.view.scale;
        let zoomed = tab.view.scale;
        frame(&mut tab, 700);
        assert_eq!(tab.view.scale, zoomed);
    }
}
