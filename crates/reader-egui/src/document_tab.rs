//! One open document: continuous layout, camera, page loading, outline,
//! presentation mode and the reading position.

use std::{
    collections::{HashMap, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Result;
use glyphst_library::DocumentPosition;
use glyphst_pdf::vector::{LayerImage, PageRect};
use glyphst_pdf_gpu::PreparedPage;

use crate::{
    document::{DocumentInfo, Layout, Loaded, Loader, OutlineEntry, PAGE_GAP, View},
    gamepad::GamepadInput,
    gpu::PageDraw,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

pub struct DocumentTab {
    pub id: u64,
    pub path: PathBuf,
    pub title: String,
    pub layout: Layout,
    pub view: View,
    /// View size in device pixels from the last frame.
    pub viewport: [f64; 2],
    fitted: bool,
    restore: Option<DocumentPosition>,
    loader: Loader,
    thumbnails: HashMap<usize, egui::TextureHandle>,
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
}

impl DocumentTab {
    pub fn open(
        id: u64,
        path: &Path,
        ctx: &egui::Context,
        restore: Option<DocumentPosition>,
    ) -> Result<Self> {
        let info = DocumentInfo::read(path)?;
        if info.bounds.is_empty() {
            anyhow::bail!("{} has no pages", path.display());
        }
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self {
            id,
            path: path.to_path_buf(),
            title: info.title.unwrap_or(file_name),
            layout: Layout::new(info.bounds),
            view: View::new(1.0),
            viewport: [1.0, 1.0],
            fitted: false,
            restore,
            loader: Loader::new(path.to_path_buf(), ctx.clone()),
            thumbnails: HashMap::new(),
            failed: HashMap::new(),
            ready: VecDeque::new(),
            ready_layers: VecDeque::new(),
            layers: HashMap::new(),
            last_view: ([0.0; 2], 0.0, None),
            outline: info.outline,
            present: None,
            load_seconds: Vec::new(),
        })
    }

    /// Takes the loader's results: thumbnails become textures, prepared
    /// pages wait in `ready` for the GPU.
    pub fn receive(&mut self, ctx: &egui::Context) {
        while let Some(loaded) = self.loader.try_recv() {
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
                    prepared,
                    layers,
                    seconds,
                } => {
                    self.load_seconds.push(seconds);
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
                Loaded::Layer { page, layer, image } => {
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
    pub fn want(&mut self, wanted: &[usize], resident: impl Fn(usize) -> bool) {
        let waiting: Vec<usize> = self.ready.iter().map(|ready| ready.page).collect();
        self.loader
            .want(wanted, |page| resident(page) || waiting.contains(&page));
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
                self.loader
                    .rasterize_layer(draw.page, index, state.ops.clone(), wanted, needed);
            }
        }
    }

    /// Device pixels per point that fit the widest page across the view.
    pub fn fit_scale(&self) -> f64 {
        self.viewport[0] / (self.layout.width + 2.0 * PAGE_GAP)
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

    /// The reading position: the page at the top of the view, the offset
    /// into it, and the zoom relative to fit width.
    pub fn position(&self) -> DocumentPosition {
        let top = self
            .layout
            .offsets
            .partition_point(|offset| offset[1] <= self.view.origin[1] + PAGE_GAP)
            .saturating_sub(1);
        DocumentPosition::fixed(
            self.view.origin[0] as f32,
            self.view.origin[1] as f32,
            (self.view.scale / self.fit_scale()) as f32,
            self.present.unwrap_or(top),
        )
    }

    fn apply_restore(&mut self, position: &DocumentPosition) {
        let page = position.page.min(self.layout.len() - 1);
        let zoom = f64::from(position.zoom);
        if zoom.is_finite() && zoom > 0.0 {
            self.view = View::new(self.fit_scale() * zoom.clamp(MIN_ZOOM, MAX_ZOOM));
        }
        self.go_to_page(page);
        // The saved offset into the page, when it is one (positions written
        // by other layouts only place the page).
        let into_page = f64::from(position.scroll_y_doc) - self.layout.offsets[page][1];
        if (0.0..self.layout.page_size(page)[1]).contains(&into_page) {
            self.view.origin[1] = self.layout.offsets[page][1] + into_page;
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
            ViewAction::GoToPage(page) => self.go_to_page(page),
        }
        None
    }

    /// Advances the view for this frame and lists what to draw.
    pub fn frame(&mut self, viewport: [u32; 2], input: ViewInput<'_>, turning: bool) -> TabFrame {
        self.viewport = [f64::from(viewport[0]), f64::from(viewport[1])];
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

        let (draws, wanted) = match self.present {
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
        };
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
        DocumentTab {
            id: 1,
            path: PathBuf::from("/nonexistent.pdf"),
            title: "t".into(),
            layout: Layout::new(vec![[0.0, 0.0, 600.0, 800.0]; pages]),
            view: View::new(1.0),
            viewport: [1.0, 1.0],
            fitted: false,
            restore: None,
            loader: Loader::new(PathBuf::from("/nonexistent.pdf"), ctx),
            thumbnails: HashMap::new(),
            failed: HashMap::new(),
            ready: VecDeque::new(),
            ready_layers: VecDeque::new(),
            layers: HashMap::new(),
            last_view: ([0.0; 2], 0.0, None),
            outline: Vec::new(),
            present: None,
            load_seconds: Vec::new(),
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
        assert_eq!(position.page, 7);

        let mut second = tab(20);
        second.restore = Some(position.clone());
        settle(&mut second);
        assert_eq!(second.position().page, 7);
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
}
