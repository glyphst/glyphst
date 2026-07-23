use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use crate::ai_panel::{AI_PANEL_WIDTH, AiPanel, AiPanelEvent};
use crate::bookmarks::{
    BookmarkPalette, BookmarkPaletteDelegate, BookmarkPrompt, BookmarkPromptDelegate,
};
use crate::go_to_page::{GoToPagePrompt, GoToPagePromptDelegate};
use crate::history::{HistoryPalette, HistoryPaletteDelegate};
use crate::icons::{Icon, IconName};
use crate::library::{
    DocumentEditRevision, DocumentPosition, Highlight, HighlightColor, HighlightRect, LibraryStore,
    Note, SemanticTextRange, SidebarWidths, normalize_path,
};
use crate::note_editor::{NoteEditor, NoteEditorEvent};
use crate::notes::{NotePaletteEntry, NotesPalette, NotesPaletteDelegate};
use crate::picker::{Picker, PickerEvent};
use crate::scrollbar::{ManualScrollHandle, WithScrollbar as _, uniform_list_scrollbar_for};
use crate::search::{SearchBar, SearchEvent};
use crate::settings::{
    BehaviorSettings, CopyFormat, PageImageQuality, PageTextFormat, SettingsStore,
    TranslationBoxCorner, WindowSettings, ui_rems,
};
use crate::smooth_scroll::advance_toward;
use crate::theme::Theme;
use crate::toc::{TocPalette, TocPaletteDelegate, TocPaletteEntry};
use anyhow::{Context as _, Result, anyhow, bail};
use arboard::{Clipboard, ImageData};
pub use glyphst_actions::pdf_viewer::*;
use glyphst_ai::ai_chat::{
    ContextBlock, ImageContext, MAX_PAGE_IMAGE_BYTES, MentionKind, MentionReference,
};
use glyphst_ai_ui::{
    AiContext, AiContextProvider, AiPageState, ResolvedPageContext, ResolvedRegionContext,
};
use glyphst_core::{
    DocumentCapabilities, DocumentFormat, DocumentLayout, DocumentRotation, EpubReaderPreferences,
    EpubTheme, PageIndex, PageRegion, RegionImageReference, SemanticTextAnchor,
};
use glyphst_pdf::{DocumentSnapshot, ReloadCoordinator, ReloadDisposition};
use glyphst_pdf_translation_app::{PdfTranslationRequest, PdfTranslationService};
use glyphst_translation::{LanguageCode, TranslationRequest, TranslationService};
use glyphst_translation_ui::{
    DocumentTranslationJobManager, DocumentTranslationOpenMode, target_language_items,
};
use glyphst_ui::{
    icons::IconButton,
    markdown::{Markdown, MarkdownElement, MarkdownStyle},
    notification_center::NotificationCenter,
    searchable_dropdown::{SearchableDropdown, SearchableDropdownEvent},
    tooltip::text_tooltip,
};
use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, AsyncApp, BorderStyle, Bounds, ClipboardItem,
    Context, Corners, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, Font,
    FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point as GpuiPoint, Render, RenderImage, ScrollDelta, ScrollHandle, ScrollStrategy,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement, Task, TextAlign, TextRun,
    TextStyle, UniformListScrollHandle, Window, WrappedLine, anchored, canvas, deferred, div, fill,
    outline, point, prelude::*, px, rgb, rgba, size, uniform_list,
};
use image::{Frame, RgbaImage, codecs::jpeg::JpegEncoder};
use mupdf::text_page::{TextBlockType, TextCharFlags};
use mupdf::{
    ColorParams, Colorspace, DestinationKind, Device, DisplayList, Image as PdfImage, Matrix,
    Pixmap, Quad, Rect, TextPageFlags,
};
use smallvec::smallvec;

const INITIAL_SELECTION_TOLERANCE_PX: f32 = 24.0;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 8.0;
const SCROLL_LINE_MULTIPLIER: f32 = 20.0;
const SCROLL_LINE_STEP_PX: f32 = 120.0;
const SMOOTH_SCROLL_LARGE_JUMP_VIEWPORTS: f32 = 3.0;
const CTRL_SCROLL_ZOOM_STEP_MULTIPLIER: f32 = 0.25;
const MAX_CACHED_RENDERS: usize = 2048;
const MAX_CACHED_RENDER_PIXELS: u64 = 30_000_000;
const MAX_PREFETCH_RENDER_PIXELS: u64 = MAX_CACHED_RENDER_PIXELS / 3;
const MAX_CACHED_DISPLAY_LISTS: usize = 50;
const MAX_PENDING_PAGE_RENDERS: usize = 6;
const MAX_RENDER_TEXTURE_DIMENSION: u32 = 8192;
const MAX_JUMP_HISTORY: usize = 128;
const JUMP_POSITION_EPSILON_DOC: f32 = 0.5;
const TOC_SIDEBAR_WIDTH: f32 = 280.0;
const TOC_SIDEBAR_ROW_HEIGHT: f32 = 30.0;
const NOTES_SIDEBAR_WIDTH: f32 = 300.0;
const NOTES_SIDEBAR_ROW_HEIGHT: f32 = 52.0;
const THUMBNAIL_SIDEBAR_WIDTH: f32 = 200.0;
const THUMBNAIL_SIDEBAR_HORIZONTAL_PADDING: f32 = 12.0;
const THUMBNAIL_SIDEBAR_THUMBNAIL_PADDING: f32 = 8.0;
const THUMBNAIL_SIDEBAR_LABEL_HEIGHT: f32 = 18.0;
const THUMBNAIL_SIDEBAR_ROW_PADDING_Y: f32 = 12.0;
const THUMBNAIL_SIDEBAR_MIN_THUMBNAIL_WIDTH: f32 = 96.0;
const THUMBNAIL_SIDEBAR_MAX_THUMBNAIL_WIDTH: f32 = 220.0;
const THUMBNAIL_SIDEBAR_COMPACT_HEADER_WIDTH: f32 = 300.0;
const SIDEBAR_RESIZE_HANDLE_SIZE: f32 = 6.0;
const SIDEBAR_MIN_WIDTH: f32 = 150.0;
const SIDEBAR_MAX_WIDTH_RATIO: f32 = 0.5;
const GRID_OVERVIEW_DEFAULT_ZOOM: f32 = 1.0;
const GRID_OVERVIEW_MIN_ZOOM: f32 = 0.25;
const GRID_OVERVIEW_MAX_ZOOM: f32 = 4.0;
const GRID_OVERVIEW_DEFAULT_COLS: usize = 4;
const GRID_OVERVIEW_BASE_THUMBNAIL_WIDTH: f32 = 130.0;
const GRID_OVERVIEW_BASE_THUMBNAIL_MAX_HEIGHT: f32 = 180.0;
const GRID_OVERVIEW_CELL_PADDING: f32 = 8.0;
const GRID_OVERVIEW_CELL_GAP: f32 = 12.0;
const GRID_OVERVIEW_ROW_PADDING_Y: f32 = 16.0;
const GRID_OVERVIEW_LABEL_HEIGHT: f32 = 18.0;
const GRID_OVERVIEW_HEADER_HEIGHT: f32 = 40.0;
const GRID_OVERVIEW_VIEWPORT_WIDTH_EPSILON: f32 = 0.5;
const PAGE_EXPORT_SCALE: f32 = 300.0 / 72.0;
const MIN_REGION_SELECTION_PX: f32 = 4.0;
const MAX_REGION_EXPORT_PIXELS: u64 = 40_000_000;
const SIDEBAR_PAGE_NUMBER_WIDTH: f32 = 38.0;
const INFERRED_SPACE_WIDTH_RATIO: f32 = 0.75;
const INFERRED_SPACE_HEIGHT_RATIO: f32 = 0.20;
const FILE_WATCH_POLL_INTERVAL: Duration = Duration::from_millis(500);
const FILE_WATCH_STABLE_DELAY: Duration = Duration::from_millis(300);
const PREVIOUS_DOCUMENT_EVICT_DELAY: Duration = Duration::from_millis(1500);
const DIFF_FLASH_DURATION: Duration = Duration::from_millis(800);
const DIFF_FLASH_TILE_SIZE: u32 = 24;
const DIFF_FLASH_PIXEL_THRESHOLD: u16 = 32;
const DIFF_FLASH_MAX_RECTS: usize = 64;
const FOOTBAR_HEIGHT: f32 = 26.0;
const FOOTBAR_BUTTON_HEIGHT: f32 = 20.0;
const FOOTBAR_ICON_SIZE: f32 = 14.0;
const MAX_CACHED_TRANSLATED_LAYOUTS: usize = 512;
const TRANSLATED_LAYOUT_KEEP_FRAMES: u64 = 120;

static NEXT_DOCUMENT_ID: AtomicU64 = AtomicU64::new(1);

fn next_document_id() -> u64 {
    NEXT_DOCUMENT_ID.fetch_add(1, Ordering::Relaxed)
}

struct PdfPage {
    bounds: Rect,
    width: f32,
    height: f32,
    offset_y: f32,
    glyph_range: Range<usize>,
}

struct PageImageExport {
    page_idx: usize,
    display_list: Arc<DisplayList>,
    bounds: Rect,
    width: f32,
    height: f32,
    rotation: DocumentRotation,
}

struct RegionImageExport {
    display_list: Arc<DisplayList>,
    page_bounds: Rect,
    region: PageRegion,
    rotation: DocumentRotation,
}

impl RegionImageExport {
    fn view_region(&self) -> Rect {
        let region = self.region.normalized();
        PdfView::pdf_rect_to_view(
            self.page_bounds,
            self.rotation,
            Rect::new(region.x0, region.y0, region.x1, region.y1),
        )
    }
}

fn clear_copied_region_selection(
    selected_region: &mut Option<RegionImageReference>,
    context_menu_region: &mut bool,
    copied_region: &RegionImageReference,
) -> bool {
    if selected_region.as_ref() != Some(copied_region) {
        return false;
    }
    *selected_region = None;
    *context_menu_region = false;
    true
}

struct PageChunk {
    start_page_idx: usize,
    pages: Vec<PdfPage>,
    max_page_width: f32,
}

struct LoadedPages {
    pages: Vec<PdfPage>,
    max_page_width: f32,
    content_height_doc: f32,
}

struct TextChunk {
    start_page_idx: usize,
    glyphs: Vec<Glyph>,
    text_blocks: Vec<TextBlock>,
    text_lines: Vec<TextLine>,
    links: Vec<PdfLink>,
    page_glyph_ranges: Vec<Range<usize>>,
    page_line_ranges: Vec<Range<usize>>,
    page_block_ranges: Vec<Range<usize>>,
    line_count: u32,
    block_count: u32,
}

struct LoadedText {
    glyphs: Vec<Glyph>,
    text_blocks: Vec<TextBlock>,
    text_lines: Vec<TextLine>,
    links: Vec<PdfLink>,
    page_glyph_ranges: Vec<Range<usize>>,
    page_line_ranges: Vec<Range<usize>>,
    page_block_ranges: Vec<Range<usize>>,
}

struct ReloadData {
    snapshot: Arc<DocumentSnapshot>,
    page_count: usize,
    loaded: LoadedPages,
    toc: Vec<TocEntry>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileSignature {
    modified: Option<SystemTime>,
    len: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct RenderKey {
    doc_id: u64,
    page_idx: usize,
    rotation: DocumentRotation,
    physical_width: u32,
    physical_height: u32,
}

impl RenderKey {
    fn pixel_count(self) -> u64 {
        self.physical_width as u64 * self.physical_height as u64
    }
}

struct CachedRender {
    key: RenderKey,
    render_scale: f32,
    last_access: u64,
    image: Arc<RenderImage>,
}

struct CachedDisplayList {
    doc_id: u64,
    page_idx: usize,
    display_list: Arc<DisplayList>,
    last_access: u64,
}

#[derive(Default)]
pub struct RenderCache {
    renders: Vec<CachedRender>,
    display_lists: Vec<CachedDisplayList>,
    protected_pages: HashSet<(u64, usize, DocumentRotation)>,
    access_counter: u64,
}

impl gpui::Global for RenderCache {}

impl RenderCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn contains_render(&self, key: RenderKey) -> bool {
        self.exact_render_index(key).is_some()
    }

    fn set_protected_pages(
        &mut self,
        pages: impl IntoIterator<Item = (u64, usize, DocumentRotation)>,
    ) {
        self.protected_pages.clear();
        self.protected_pages.extend(pages);
    }

    fn render(&mut self, key: RenderKey) -> Option<Arc<RenderImage>> {
        self.exact_render_index(key)
            .map(|index| self.touch_render(index))
    }

    fn scaled_render(
        &mut self,
        doc_id: u64,
        page_idx: usize,
        rotation: DocumentRotation,
        render_scale: f32,
    ) -> Option<Arc<RenderImage>> {
        let index = self
            .renders
            .iter()
            .enumerate()
            .filter(|(_, cached)| {
                cached.key.doc_id == doc_id
                    && cached.key.page_idx == page_idx
                    && cached.key.rotation == rotation
            })
            .min_by(|(_, a), (_, b)| {
                (a.render_scale - render_scale)
                    .abs()
                    .total_cmp(&(b.render_scale - render_scale).abs())
            })
            .map(|(index, _)| index)?;

        Some(self.touch_render(index))
    }

    fn insert_render(
        &mut self,
        key: RenderKey,
        render_scale: f32,
        image: Arc<RenderImage>,
    ) -> Vec<Arc<RenderImage>> {
        let last_access = self.next_access();
        let mut removed = Vec::new();

        if let Some(index) = self.exact_render_index(key) {
            let replaced = std::mem::replace(
                &mut self.renders[index],
                CachedRender {
                    key,
                    render_scale,
                    last_access,
                    image,
                },
            );
            removed.push(replaced.image);
        } else {
            self.renders.push(CachedRender {
                key,
                render_scale,
                last_access,
                image,
            });
        }

        while self.renders.len() > MAX_CACHED_RENDERS
            || (self.renders.len() > 1 && self.render_pixels() > MAX_CACHED_RENDER_PIXELS)
        {
            if let Some(index) = self
                .renders
                .iter()
                .enumerate()
                .filter(|(_, cached)| {
                    !self.protected_pages.contains(&(
                        cached.key.doc_id,
                        cached.key.page_idx,
                        cached.key.rotation,
                    ))
                })
                .min_by_key(|(_, cached)| cached.last_access)
                .map(|(index, _)| index)
            {
                removed.push(self.renders.remove(index).image);
            } else {
                break;
            }
        }

        removed
    }

    fn display_list(&mut self, doc_id: u64, page_idx: usize) -> Option<Arc<DisplayList>> {
        let index = self
            .display_lists
            .iter()
            .position(|cached| cached.doc_id == doc_id && cached.page_idx == page_idx)?;
        let last_access = self.next_access();
        self.display_lists[index].last_access = last_access;
        Some(self.display_lists[index].display_list.clone())
    }

    fn insert_display_list(
        &mut self,
        doc_id: u64,
        page_idx: usize,
        display_list: Arc<DisplayList>,
    ) -> Arc<DisplayList> {
        let last_access = self.next_access();

        if let Some(index) = self
            .display_lists
            .iter()
            .position(|cached| cached.doc_id == doc_id && cached.page_idx == page_idx)
        {
            self.display_lists[index] = CachedDisplayList {
                doc_id,
                page_idx,
                display_list: display_list.clone(),
                last_access,
            };
        } else {
            self.display_lists.push(CachedDisplayList {
                doc_id,
                page_idx,
                display_list: display_list.clone(),
                last_access,
            });
        }

        while self.display_lists.len() > MAX_CACHED_DISPLAY_LISTS {
            if let Some(index) = self
                .display_lists
                .iter()
                .enumerate()
                .min_by_key(|(_, cached)| cached.last_access)
                .map(|(index, _)| index)
            {
                self.display_lists.remove(index);
            } else {
                break;
            }
        }

        display_list
    }

    fn evict_document(&mut self, doc_id: u64) -> Vec<Arc<RenderImage>> {
        let mut removed = Vec::new();
        self.renders.retain(|cached| {
            if cached.key.doc_id == doc_id {
                removed.push(cached.image.clone());
                false
            } else {
                true
            }
        });
        self.display_lists.retain(|cached| cached.doc_id != doc_id);
        removed
    }

    fn exact_render_index(&self, key: RenderKey) -> Option<usize> {
        self.renders.iter().position(|cached| cached.key == key)
    }

    fn touch_render(&mut self, index: usize) -> Arc<RenderImage> {
        let last_access = self.next_access();
        self.renders[index].last_access = last_access;
        self.renders[index].image.clone()
    }

    fn render_pixels(&self) -> u64 {
        self.renders
            .iter()
            .map(|cached| cached.key.pixel_count())
            .sum()
    }

    fn next_access(&mut self) -> u64 {
        self.access_counter = self.access_counter.wrapping_add(1);
        self.access_counter
    }
}

struct PendingRender {
    key: RenderKey,
    _task: Task<()>,
}

struct CollectedImage {
    bounds: Rect,
    matrix: Matrix,
    image: PdfImage,
}

#[derive(Default)]
struct ImageCollector {
    images: Vec<CollectedImage>,
}

impl ImageCollector {
    fn transformed_unit_square_bounds(matrix: &Matrix) -> Rect {
        let corners = [
            matrix.transform_xy(0.0, 0.0),
            matrix.transform_xy(1.0, 0.0),
            matrix.transform_xy(0.0, 1.0),
            matrix.transform_xy(1.0, 1.0),
        ];

        let (mut x0, mut y0) = corners[0];
        let (mut x1, mut y1) = corners[0];
        for (x, y) in corners.into_iter().skip(1) {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }

        Rect::new(x0, y0, x1, y1)
    }
}

impl mupdf::NativeDevice for ImageCollector {
    fn fill_image(&mut self, image: &PdfImage, matrix: Matrix, _alpha: f32, _cp: ColorParams) {
        let bounds = Self::transformed_unit_square_bounds(&matrix);
        self.images.push(CollectedImage {
            bounds,
            matrix,
            image: image.clone(),
        });
    }
}

struct PaintPage {
    bounds: Bounds<Pixels>,
    image: Option<Arc<RenderImage>>,
}

type HighlightOverlay = (Bounds<Pixels>, HighlightColor);

#[derive(Clone, Copy)]
struct DiffFlash {
    page_idx: usize,
    rect: Rect,
    started_at: Instant,
}

#[derive(Clone, Copy)]
struct DiffRenderInput {
    key: RenderKey,
    page_bounds: Rect,
    page_width: f32,
    page_height: f32,
    rotation: DocumentRotation,
}

struct DiffFlashOverlay {
    bounds: Bounds<Pixels>,
    alpha: u8,
}

struct PaintData {
    pages: Vec<PaintPage>,
    translated_text_overlays: Vec<TranslatedTextOverlay>,
    diff_flash_overlays: Vec<DiffFlashOverlay>,
    highlight_overlays: Vec<HighlightOverlay>,
    selected_highlight_borders: Vec<Bounds<Pixels>>,
    note_underlines: Vec<(Bounds<Pixels>, HighlightColor)>,
    link_highlights: Vec<Bounds<Pixels>>,
    linked_hover_highlights: Vec<Bounds<Pixels>>,
    linked_selection_highlights: Vec<Bounds<Pixels>>,
    selection_highlights: Vec<Bounds<Pixels>>,
    search_highlights: Vec<Bounds<Pixels>>,
    active_search_highlights: Vec<Bounds<Pixels>>,
    region_overlays: Vec<Bounds<Pixels>>,
}

#[derive(Clone)]
struct Glyph {
    c: char,
    quad: Quad,
    line_rect: Rect,
    page_idx: usize,
    line_idx: u32,
    block_idx: u32,
    font_size: f32,
    color: u32,
    is_bold: bool,
    inferred_space_after: bool,
}

#[derive(Clone)]
struct TextLine {
    glyph_range: Range<usize>,
    bounds: Rect,
}

#[derive(Clone)]
struct TextBlock {
    id: u32,
    page_idx: usize,
    glyph_range: Range<usize>,
    bounds: Rect,
    text: String,
    font_size: f32,
    color: u32,
    horizontal: bool,
}

#[derive(Clone)]
struct TranslatedBlock {
    source: TextBlock,
    text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PdfViewLanguage {
    Original,
    Translated,
}

#[derive(Clone, Copy)]
pub struct LinkedViewport {
    pub page_idx: usize,
    pub y_in_page: f32,
    pub zoom: f32,
    pub rotation: DocumentRotation,
    pub generation: u64,
}

#[derive(Clone, Copy)]
pub struct LinkedBlockAnchor {
    pub block_id: u32,
    pub page_idx: usize,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

#[derive(Clone, Copy)]
pub struct LinkedBlockSelection {
    pub anchor: LinkedBlockAnchor,
    pub generation: u64,
}

struct TranslatedTextOverlay {
    mask: Bounds<Pixels>,
    bounds: Bounds<Pixels>,
    prepared: Arc<PreparedTranslatedText>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TranslatedTextLayoutKey {
    block_id: u32,
    text: SharedString,
    font: Font,
    color: u32,
    font_size_bits: u32,
    width_bits: u32,
    height_bits: u32,
}

struct PreparedTranslatedText {
    lines: Vec<Arc<WrappedLine>>,
    line_height: Pixels,
}

struct CachedTranslatedTextLayout {
    prepared: Arc<PreparedTranslatedText>,
    last_used_frame: u64,
}

#[derive(Clone, Copy)]
struct Selection {
    anchor: usize,
    head: usize,
}

#[derive(Clone, Copy, Debug)]
enum HighlightColorAction {
    Create,
    Change,
}

#[derive(Clone, Copy)]
struct ContextMenuState {
    has_page: bool,
    has_image: bool,
    has_selection: bool,
    highlight_color: Option<HighlightColor>,
    has_note: bool,
    has_region: bool,
    grid_overview: bool,
    copy_submenu_open: bool,
}

#[derive(Clone, Copy)]
struct RegionDragState {
    anchor: mupdf::Point,
    current: mupdf::Point,
}

#[derive(Clone)]
enum TranslationStatus {
    Idle,
    Loading,
    Ready {
        text: SharedString,
        markdown: Entity<Markdown>,
    },
    Error(SharedString),
}

#[derive(Clone)]
struct TranslationCardState {
    source: SharedString,
    status: TranslationStatus,
    position: Option<GpuiPoint<Pixels>>,
    request_generation: u64,
}

impl TranslationCardState {
    fn new(
        source: impl Into<SharedString>,
        status: TranslationStatus,
        position: Option<GpuiPoint<Pixels>>,
        request_generation: u64,
    ) -> Self {
        Self {
            source: source.into(),
            status,
            position,
            request_generation,
        }
    }
}

fn reset_translation_cards_for_document_replacement(
    popup: &mut Option<TranslationCardState>,
    pinned: &mut Option<TranslationCardState>,
    request_generation: u64,
) {
    *popup = None;
    if pinned.is_some() {
        *pinned = Some(TranslationCardState::new(
            SharedString::default(),
            TranslationStatus::Idle,
            None,
            request_generation,
        ));
    }
}

fn retain_valid_annotation_id(id: &mut Option<u64>, valid_ids: &HashSet<u64>) -> bool {
    if id.is_some_and(|id| !valid_ids.contains(&id)) {
        *id = None;
        true
    } else {
        false
    }
}

enum NoteTarget {
    New {
        rects: Vec<HighlightRect>,
        semantic_range: Option<SemanticTextRange>,
        text: String,
    },
    Edit {
        id: u64,
    },
}

pub struct PdfAnnotations {
    pub highlights: Vec<Highlight>,
    pub notes: Vec<Note>,
}

pub struct PdfOpenOptions {
    pub initial_position: Option<DocumentPosition>,
    pub sidebar_widths: Option<SidebarWidths>,
    pub annotations: PdfAnnotations,
    pub target_language: String,
    pub epub_preferences: EpubReaderPreferences,
    pub rotation: DocumentRotation,
    pub document_translation_jobs: Entity<DocumentTranslationJobManager>,
}

pub struct PreparedPdfOpen {
    path: PathBuf,
    file_name: SharedString,
    format: DocumentFormat,
    capabilities: DocumentCapabilities,
    document_layout: DocumentLayout,
    epub_preferences: EpubReaderPreferences,
    document_snapshot: Arc<DocumentSnapshot>,
    pages: Vec<PdfPage>,
    page_count: usize,
    max_page_width: f32,
    content_height_doc: f32,
    toc: Vec<TocEntry>,
}

pub enum PdfViewEvent {
    OpenDocument {
        path: PathBuf,
        position: Option<DocumentPosition>,
    },
    OpenSettings,
    ViewportChanged(LinkedViewport),
    BlockHovered(Option<LinkedBlockAnchor>),
    BlockSelected(Option<LinkedBlockSelection>),
}

struct SearchMatch {
    page_idx: usize,
    range: Range<usize>,
}

struct SearchState {
    query: SharedString,
    matches: Vec<SearchMatch>,
    active: Option<usize>,
    ignore_case: bool,
    reveal_after_indexing: bool,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: "".into(),
            matches: Vec::new(),
            active: None,
            ignore_case: true,
            reveal_after_indexing: false,
        }
    }
}

impl SearchState {
    fn update_reveal_after_indexing(&mut self, indexing: bool) {
        self.reveal_after_indexing = indexing && !self.query.is_empty();
    }

    fn cancel_reveal_after_indexing(&mut self) {
        self.reveal_after_indexing = false;
    }

    fn take_reveal_after_indexing(&mut self) -> bool {
        std::mem::take(&mut self.reveal_after_indexing)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TextIndexState {
    #[default]
    Pending,
    Indexing,
    Ready,
    Failed,
}

impl TextIndexState {
    fn begin(&mut self) -> bool {
        if *self != Self::Pending {
            return false;
        }

        *self = Self::Indexing;
        true
    }

    fn settle(&mut self, succeeded: bool) {
        *self = if succeeded { Self::Ready } else { Self::Failed };
    }

    fn is_pending_or_indexing(self) -> bool {
        matches!(self, Self::Pending | Self::Indexing)
    }
}

#[derive(Clone)]
enum LinkTarget {
    Internal {
        page_idx: usize,
        target_top: Option<f32>,
        target_left: Option<f32>,
    },
    External(String),
}

struct PdfLink {
    page_idx: usize,
    rects: Vec<Rect>,
    target: LinkTarget,
}

struct TocEntry {
    title: SharedString,
    depth: usize,
    has_children: bool,
    page_idx: usize,
    target_top: Option<f32>,
    target_left: Option<f32>,
    navigable: bool,
}

#[derive(Clone, Copy)]
struct PanState {
    pointer_origin: GpuiPoint<Pixels>,
    scroll_origin_x: f32,
    scroll_origin_y: f32,
}

#[derive(Clone, Copy)]
struct JumpPosition {
    scroll_x_doc: f32,
    scroll_y_doc: f32,
}

#[derive(Clone, Copy)]
enum DraggedSidebar {
    Thumbnails,
    Toc,
    Notes,
    AiPanel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeftSidebar {
    Thumbnails,
    Toc,
    Notes,
}

#[derive(Clone, Copy)]
enum PageSelectionSource {
    GridOverview,
    ThumbnailSidebar,
}

struct EpubStepperConfig {
    label: SharedString,
    value: String,
    decrease_id: &'static str,
    increase_id: &'static str,
}

impl Render for DraggedSidebar {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub struct PdfView {
    doc_id: u64,
    path: PathBuf,
    file_name: SharedString,
    format: DocumentFormat,
    capabilities: DocumentCapabilities,
    document_layout: DocumentLayout,
    epub_preferences: EpubReaderPreferences,
    epub_style_panel_position: Option<GpuiPoint<Pixels>>,
    document_snapshot: Arc<DocumentSnapshot>,
    pages: Vec<PdfPage>,
    page_count: usize,
    max_page_width: f32,
    content_height_doc: f32,
    glyphs: Vec<Glyph>,
    text_blocks: Vec<TextBlock>,
    text_lines: Vec<TextLine>,
    page_line_ranges: Vec<Range<usize>>,
    page_block_ranges: Vec<Range<usize>>,
    translated_blocks: BTreeMap<u32, TranslatedBlock>,
    translated_text_layouts: HashMap<TranslatedTextLayoutKey, CachedTranslatedTextLayout>,
    translated_text_layout_frame: u64,
    view_language: PdfViewLanguage,
    linked_viewport_generation: u64,
    linked_selection_generation: u64,
    suppress_linked_events: bool,
    external_hovered_block: Option<u32>,
    external_selected_block: Option<u32>,
    hovered_text_block: Option<u32>,
    translation_document_generation: u64,
    translation_document_task: Option<Task<()>>,
    links: Vec<PdfLink>,
    highlights: Vec<Highlight>,
    notes: Vec<Note>,
    selected_highlight: Option<u64>,
    selection: Option<Selection>,
    region_tool_active: bool,
    region_drag: Option<RegionDragState>,
    selected_region: Option<RegionImageReference>,
    translation_popup: Option<TranslationCardState>,
    pinned_translation: Option<TranslationCardState>,
    translation_popup_scroll: ScrollHandle,
    pinned_translation_scroll: ScrollHandle,
    target_language: String,
    translation_popup_language_dropdown: Option<Entity<SearchableDropdown>>,
    pinned_translation_language_dropdown: Option<Entity<SearchableDropdown>>,
    translation_request_generation: u64,
    document_translation_jobs: Entity<DocumentTranslationJobManager>,
    pending_selection_anchor: Option<usize>,
    dragging: bool,
    hovered_link: Option<usize>,
    hovered_note: Option<u64>,
    hovered_note_position: Option<GpuiPoint<Pixels>>,
    pending_link: Option<usize>,
    panning: Option<PanState>,
    page_image_bounds: Vec<Option<Bounds<Pixels>>>,
    focus_handle: FocusHandle,
    pending_renders: Vec<PendingRender>,
    rotation: DocumentRotation,
    zoom: f32,
    base_scale: f32,
    viewport_width: f32,
    viewport_height: f32,
    scroll_x_doc: f32,
    scroll_y_doc: f32,
    target_scroll_x_doc: f32,
    target_scroll_y_doc: f32,
    document_scroll: ManualScrollHandle,
    ai_panel: Option<Entity<AiPanel>>,
    quick_ask: Option<Entity<AiPanel>>,
    quick_ask_visible: bool,
    search_bar: Option<Entity<SearchBar>>,
    toc_palette: Option<Entity<TocPalette>>,
    notes_palette: Option<Entity<NotesPalette>>,
    history_palette: Option<Entity<HistoryPalette>>,
    bookmark_palette: Option<Entity<BookmarkPalette>>,
    bookmark_prompt: Option<Entity<BookmarkPrompt>>,
    go_to_page_prompt: Option<Entity<GoToPagePrompt>>,
    note_editor: Option<Entity<NoteEditor>>,
    note_target: Option<NoteTarget>,
    toc: Vec<TocEntry>,
    toc_collapsed: HashSet<usize>,
    toc_sidebar_open: bool,
    toc_sidebar_width: f32,
    toc_sidebar_scroll: UniformListScrollHandle,
    toc_sidebar_visible_entry_ixs: Vec<usize>,
    toc_sidebar_entries_dirty: bool,
    toc_sidebar_selected: Option<usize>,
    notes_sidebar_open: bool,
    notes_sidebar_width: f32,
    notes_sidebar_scroll: UniformListScrollHandle,
    notes_sidebar_entries: Vec<NotePaletteEntry>,
    notes_sidebar_selected: Option<u64>,
    thumbnail_sidebar_open: bool,
    thumbnail_sidebar_width: f32,
    thumbnail_sidebar_scroll: UniformListScrollHandle,
    thumbnail_sidebar_current_page: usize,
    thumbnail_sidebar_selecting: bool,
    thumbnail_sidebar_selection: BTreeSet<usize>,
    thumbnail_sidebar_actions_position: Option<GpuiPoint<Pixels>>,
    ai_panel_width: f32,
    sidebar_width_save_task: Option<Task<()>>,
    grid_overview_open: bool,
    grid_overview_scroll: UniformListScrollHandle,
    grid_overview_zoom: f32,
    grid_overview_cols: usize,
    grid_overview_cursor: usize,
    grid_overview_viewport_width: f32,
    grid_overview_selecting: bool,
    grid_overview_selection: BTreeSet<usize>,
    grid_overview_actions_position: Option<GpuiPoint<Pixels>>,
    presentation_page: Option<usize>,
    jump_back: Vec<JumpPosition>,
    jump_forward: Vec<JumpPosition>,
    position_from_search: bool,
    transparency_enabled: bool,
    window_settings: WindowSettings,
    search: SearchState,
    loading: bool,
    loading_error: Option<String>,
    pending_initial_position: Option<DocumentPosition>,
    pending_semantic_position: Option<DocumentPosition>,
    loading_task: Option<Task<()>>,
    reflow_resize_task: Option<Task<()>>,
    file_watch_task: Option<Task<()>>,
    reload_task: Option<Task<()>>,
    reload_coordinator: ReloadCoordinator,
    previous_doc_id: Option<u64>,
    previous_doc_evict_task: Option<Task<()>>,
    diff_flashes: Vec<DiffFlash>,
    diff_pending_pages: BTreeSet<usize>,
    diff_flash_clear_task: Option<Task<()>>,
    text_index_state: TextIndexState,
    text_indexing_task: Option<Task<()>>,
    position_save_task: Option<Task<()>>,
    context_menu_position: Option<GpuiPoint<Pixels>>,
    context_menu_page: Option<usize>,
    context_menu_image: Option<(usize, usize)>,
    context_menu_highlight: Option<u64>,
    context_menu_note: Option<u64>,
    context_menu_region: bool,
    context_menu_copy_submenu_open: bool,
    document_edit_revision: DocumentEditRevision,
    bookmarks_revision: u64,
    _image_clipboard: Option<Clipboard>,
}

pub type DocumentView = PdfView;

#[derive(Clone)]
struct PdfViewAiContextProvider {
    pdf_view: gpui::WeakEntity<PdfView>,
}

#[derive(Clone, Debug)]
struct TextFormatBlock {
    lines: Vec<String>,
    average_font_size: f32,
    bold_ratio: f32,
    layout: Option<TextFormatLayout>,
}

#[derive(Clone, Copy, Debug)]
struct TextFormatLayout {
    page_idx: usize,
    first_line: Rect,
    last_line: Rect,
}

impl AiContextProvider for PdfViewAiContextProvider {
    fn context(&self, include_document_text: bool, cx: &gpui::App) -> Option<AiContext> {
        self.pdf_view
            .read_with(cx, |pdf_view, _| AiContext {
                document_file_name: pdf_view
                    .path
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .map(str::to_string),
                document_format: Some(
                    match pdf_view.format {
                        DocumentFormat::Pdf => "PDF",
                        DocumentFormat::Epub => "EPUB",
                    }
                    .to_string(),
                ),
                location_label: pdf_view.capabilities.reflow.then(|| {
                    let progress = if pdf_view.max_scroll_y_doc() > 0.0 {
                        pdf_view.target_scroll_y_doc / pdf_view.max_scroll_y_doc()
                    } else {
                        0.0
                    };
                    format!("Reading progress: {:.0}%", progress.clamp(0.0, 1.0) * 100.0)
                }),
                page_count: if pdf_view.capabilities.reflow {
                    0
                } else {
                    pdf_view.page_count()
                },
                current_page: if pdf_view.capabilities.reflow {
                    0
                } else {
                    pdf_view.current_page()
                },
                selected_text: pdf_view.selected_text(),
                selected_region: pdf_view.selected_region.clone(),
                document_text: include_document_text
                    .then(|| pdf_view.document_text())
                    .flatten(),
            })
            .ok()
    }

    fn page_state(&self, cx: &gpui::App) -> Option<AiPageState> {
        self.pdf_view
            .read_with(cx, |pdf_view, _| pdf_view.ai_page_state())
            .ok()
    }

    fn resolve_page_references(
        &self,
        references: Vec<MentionReference>,
        cx: &gpui::App,
    ) -> Task<Result<Vec<ResolvedPageContext>>> {
        let inputs = self
            .pdf_view
            .read_with(cx, |pdf_view, cx| {
                let ai_settings = &cx.global::<SettingsStore>().settings.ai;
                let references = PdfView::prepare_page_references(
                    references,
                    pdf_view.document_layout,
                    pdf_view.page_count,
                )?;
                Ok((
                    pdf_view.document_snapshot.clone(),
                    pdf_view.document_layout,
                    pdf_view.page_count,
                    ai_settings.page_text_format,
                    ai_settings.page_image_quality,
                    pdf_view.rotation,
                    references,
                ))
            })
            .map_err(|error| anyhow!("failed to access PDF page context: {error}"))
            .and_then(|inputs| inputs);

        cx.background_spawn(async move {
            let (
                snapshot,
                document_layout,
                page_count,
                text_format,
                image_quality,
                rotation,
                references,
            ) = inputs?;
            PdfView::resolve_page_contexts(
                &snapshot,
                document_layout,
                page_count,
                text_format,
                image_quality,
                rotation,
                references,
            )
        })
    }

    fn resolve_region_references(
        &self,
        references: Vec<RegionImageReference>,
        cx: &gpui::App,
    ) -> Task<Result<Vec<ResolvedRegionContext>>> {
        let inputs = self
            .pdf_view
            .read_with(cx, |pdf_view, cx| {
                let store = cx.global::<SettingsStore>();
                Ok((
                    pdf_view.document_snapshot.clone(),
                    pdf_view.document_layout,
                    pdf_view.page_count,
                    store.settings.ai.page_image_quality,
                    store.settings.ai.page_image_limit,
                    pdf_view.rotation,
                    references,
                ))
            })
            .map_err(|error| anyhow!("failed to access PDF region context: {error}"))
            .and_then(|inputs| inputs);

        cx.background_spawn(async move {
            let (
                snapshot,
                document_layout,
                page_count,
                image_quality,
                page_image_limit,
                rotation,
                references,
            ) = inputs?;
            PdfView::resolve_region_contexts(
                &snapshot,
                document_layout,
                page_count,
                image_quality,
                page_image_limit,
                rotation,
                references,
            )
        })
    }
}

impl PdfView {
    fn ai_page_state(&self) -> AiPageState {
        if self.capabilities.reflow {
            AiPageState::default()
        } else {
            AiPageState {
                page_count: self.page_count(),
                current_page: self.current_page(),
            }
        }
    }

    // This resolver owns page-scoped mentions and expands document-image mentions.
    // Selection and document-text mentions are resolved synchronously by the AI
    // panel and are ignored here.
    fn prepare_page_references(
        references: Vec<MentionReference>,
        document_layout: DocumentLayout,
        page_count: usize,
    ) -> Result<Vec<MentionReference>> {
        let page_references = references
            .into_iter()
            .filter(|reference| reference.kind.is_page_reference())
            .collect::<Vec<_>>();
        if page_references.is_empty() {
            return Ok(Vec::new());
        }
        if !matches!(document_layout, DocumentLayout::Fixed) {
            bail!("page references are unavailable for reflowable documents");
        }

        let mut unique = Vec::with_capacity(page_references.len());
        let mut seen = HashSet::with_capacity(page_references.len());
        for reference in page_references {
            if reference.kind == MentionKind::DocumentImage {
                if reference.page.is_some() {
                    bail!("{} does not accept a page number", reference.kind.token());
                }
                for page in 1..=page_count {
                    let page_reference =
                        MentionReference::new(MentionKind::DocumentImage, Some(page));
                    if seen.insert(page_reference) {
                        unique.push(page_reference);
                    }
                }
                continue;
            }

            let page = reference.page.ok_or_else(|| {
                anyhow!("{} requires a 1-based page number", reference.kind.token())
            })?;
            if page == 0 || page > page_count {
                bail!(
                    "invalid page reference {}: page must be between 1 and {}",
                    reference.display_text(),
                    page_count
                );
            }
            if seen.insert(reference) {
                unique.push(reference);
            }
        }
        Ok(unique)
    }

    #[ztracing::instrument(skip_all, fields(reference_count = references.len()))]
    fn resolve_page_contexts(
        snapshot: &DocumentSnapshot,
        document_layout: DocumentLayout,
        expected_page_count: usize,
        text_format: PageTextFormat,
        image_quality: PageImageQuality,
        rotation: DocumentRotation,
        references: Vec<MentionReference>,
    ) -> Result<Vec<ResolvedPageContext>> {
        if references.is_empty() {
            return Ok(Vec::new());
        }
        if !matches!(document_layout, DocumentLayout::Fixed) {
            bail!("page references are unavailable for reflowable documents");
        }

        let opened = snapshot
            .open_with_layout(document_layout)
            .context("failed to open document for page context")?;
        if opened.is_reflowable() {
            bail!("page references are unavailable for reflowable documents");
        }
        let document = opened.into_inner();
        let actual_page_count = document
            .page_count()
            .context("failed to read document page count for page context")?;
        if actual_page_count < 0 || actual_page_count as usize != expected_page_count {
            bail!(
                "document page count changed while resolving page context: expected {}, got {}",
                expected_page_count,
                actual_page_count
            );
        }

        let mut resolved = Vec::with_capacity(references.len());
        let mut page_groups: Vec<(usize, Vec<MentionReference>)> = Vec::new();
        let mut page_group_indexes = HashMap::new();
        for reference in references {
            let page_number = reference
                .page
                .context("validated page reference lost its page number")?;
            let group_ix = *page_group_indexes.entry(page_number).or_insert_with(|| {
                page_groups.push((page_number, Vec::new()));
                page_groups.len() - 1
            });
            page_groups[group_ix].1.push(reference);
        }

        let mut rendered_image_bytes = 0usize;
        for (page_number, page_references) in page_groups {
            let page_idx = page_number - 1;
            let page = document
                .load_page(page_idx as i32)
                .with_context(|| format!("failed to load PDF page {page_number}"))?;
            let page_bounds = page
                .bounds()
                .with_context(|| format!("failed to read bounds for PDF page {page_number}"))?;
            let page_width = page_bounds.x1 - page_bounds.x0;
            let page_height = page_bounds.y1 - page_bounds.y0;
            let display_list = DisplayList::new(page_bounds).with_context(|| {
                format!("failed to create display list for PDF page {page_number}")
            })?;
            let device = Device::from_display_list(&display_list).with_context(|| {
                format!("failed to create display-list device for PDF page {page_number}")
            })?;
            page.run(&device, &Matrix::IDENTITY)
                .with_context(|| format!("failed to record PDF page {page_number}"))?;

            let markdown = page_references
                .iter()
                .any(|reference| reference.kind == MentionKind::Page)
                .then(|| Self::extract_page_markdown(&display_list, text_format))
                .transpose()
                .with_context(|| format!("failed to extract text from PDF page {page_number}"))?;
            let image_bytes = if page_references
                .iter()
                .any(|reference| reference.kind.is_image())
            {
                let bytes = Self::render_page_jpeg(
                    &display_list,
                    page_bounds,
                    page_width,
                    page_height,
                    image_quality,
                    rotation,
                )
                .with_context(|| format!("failed to render PDF page {page_number} as JPEG"))?;
                rendered_image_bytes = rendered_image_bytes.saturating_add(bytes.len());
                if rendered_image_bytes > MAX_PAGE_IMAGE_BYTES {
                    bail!(
                        "attached page images are too large: more than {} MiB",
                        MAX_PAGE_IMAGE_BYTES / (1024 * 1024)
                    );
                }
                Some(Arc::<[u8]>::from(bytes))
            } else {
                None
            };

            for reference in page_references {
                match reference.kind {
                    MentionKind::Page => {
                        resolved.push(ResolvedPageContext::Text(ContextBlock::new(
                            MentionKind::Page,
                            Some(page_number),
                            markdown.clone().unwrap_or_default(),
                        )));
                    }
                    MentionKind::PageImage | MentionKind::DocumentImage => {
                        resolved.push(ResolvedPageContext::Image(ImageContext::new(
                            reference.kind,
                            Some(page_number),
                            "image/jpeg",
                            image_bytes.clone().context("page image was not rendered")?,
                        )));
                    }
                    MentionKind::Selection | MentionKind::DocumentText => {
                        unreachable!(
                            "non-page references are filtered before background resolution"
                        )
                    }
                    MentionKind::RegionImage => {
                        unreachable!("region references use the region resolver")
                    }
                }
            }
        }

        Ok(resolved)
    }

    #[ztracing::instrument(skip_all, fields(reference_count = references.len()))]
    fn resolve_region_contexts(
        snapshot: &DocumentSnapshot,
        document_layout: DocumentLayout,
        expected_page_count: usize,
        image_quality: PageImageQuality,
        page_image_limit: usize,
        rotation: DocumentRotation,
        references: Vec<RegionImageReference>,
    ) -> Result<Vec<ResolvedRegionContext>> {
        if references.is_empty() {
            return Ok(Vec::new());
        }
        if !matches!(document_layout, DocumentLayout::Fixed) {
            bail!("region images are unavailable for reflowable documents");
        }
        let region_page_count = references
            .iter()
            .map(|reference| reference.regions.len())
            .sum::<usize>();
        if region_page_count > page_image_limit {
            bail!(
                "too many pages attached through region images: {region_page_count} requested, maximum is {page_image_limit}"
            );
        }

        let opened = snapshot
            .open_with_layout(document_layout)
            .context("failed to open document for region image context")?;
        if opened.is_reflowable() {
            bail!("region images are unavailable for reflowable documents");
        }
        let document = opened.into_inner();
        let actual_page_count = document
            .page_count()
            .context("failed to read document page count for region image context")?;
        if actual_page_count < 0 || actual_page_count as usize != expected_page_count {
            bail!(
                "document page count changed while resolving region images: expected {}, got {}",
                expected_page_count,
                actual_page_count
            );
        }

        let mut display_lists = HashMap::<usize, Arc<DisplayList>>::new();
        let mut page_bounds_by_index = HashMap::<usize, Rect>::new();
        let mut resolved = Vec::with_capacity(references.len());
        let mut total_bytes = 0usize;
        for reference in references {
            if reference.is_empty() {
                bail!("PDF image region is empty");
            }
            let mut exports = Vec::with_capacity(reference.regions.len());
            for region in &reference.regions {
                let page_idx = region.page.0;
                if page_idx >= expected_page_count {
                    bail!("PDF image region references missing page {}", page_idx + 1);
                }
                let display_list = if let Some(display_list) = display_lists.get(&page_idx) {
                    display_list.clone()
                } else {
                    let page = document
                        .load_page(page_idx as i32)
                        .with_context(|| format!("failed to load PDF page {}", page_idx + 1))?;
                    let page_bounds = page.bounds().with_context(|| {
                        format!("failed to read bounds for PDF page {}", page_idx + 1)
                    })?;
                    let display_list = DisplayList::new(page_bounds).with_context(|| {
                        format!(
                            "failed to create display list for PDF page {}",
                            page_idx + 1
                        )
                    })?;
                    let device = Device::from_display_list(&display_list).with_context(|| {
                        format!(
                            "failed to create display-list device for PDF page {}",
                            page_idx + 1
                        )
                    })?;
                    page.run(&device, &Matrix::IDENTITY)
                        .with_context(|| format!("failed to record PDF page {}", page_idx + 1))?;
                    let display_list = Arc::new(display_list);
                    page_bounds_by_index.insert(page_idx, page_bounds);
                    display_lists.insert(page_idx, display_list.clone());
                    display_list
                };
                let page_bounds = page_bounds_by_index
                    .get(&page_idx)
                    .copied()
                    .context("PDF page bounds were not cached")?;
                let normalized = region.normalized();
                if !normalized.x0.is_finite()
                    || !normalized.y0.is_finite()
                    || !normalized.x1.is_finite()
                    || !normalized.y1.is_finite()
                    || normalized.x0 < page_bounds.x0
                    || normalized.y0 < page_bounds.y0
                    || normalized.x1 > page_bounds.x1
                    || normalized.y1 > page_bounds.y1
                    || normalized.x1 <= normalized.x0
                    || normalized.y1 <= normalized.y0
                {
                    bail!("PDF image region is outside page {} bounds", page_idx + 1);
                }
                exports.push(RegionImageExport {
                    display_list,
                    page_bounds,
                    region: *region,
                    rotation,
                });
            }

            let bytes = Self::render_region_jpeg(&exports, image_quality)
                .context("failed to render PDF image region")?;
            total_bytes = total_bytes.saturating_add(bytes.len());
            if total_bytes > MAX_PAGE_IMAGE_BYTES {
                bail!(
                    "attached page images are too large: more than {} MiB",
                    MAX_PAGE_IMAGE_BYTES / (1024 * 1024)
                );
            }
            let image = ImageContext::new_region(reference.clone(), "image/jpeg", bytes);
            resolved.push(ResolvedRegionContext { reference, image });
        }

        Ok(resolved)
    }

    fn extract_page_markdown(display_list: &DisplayList, format: PageTextFormat) -> Result<String> {
        let mut flags = TextPageFlags::PRESERVE_WHITESPACE | TextPageFlags::ACCURATE_BBOXES;
        if format == PageTextFormat::Semantic {
            flags |= TextPageFlags::PARAGRAPH_BREAK | TextPageFlags::TABLE_HUNT;
        }
        let text_page = display_list
            .to_text_page(flags)
            .context("failed to create MuPDF structured text page")?;
        let mut blocks = Vec::new();

        for block in text_page.blocks() {
            if block.r#type() != TextBlockType::Text {
                continue;
            }

            let mut lines = Vec::new();
            let mut total_font_size = 0.0_f32;
            let mut character_count = 0_usize;
            let mut bold_count = 0_usize;
            for line in block.lines() {
                let mut text = String::new();
                for ch in line.chars() {
                    let Some(c) = ch.char() else {
                        continue;
                    };
                    text.push(c);
                    if !c.is_whitespace() {
                        total_font_size += ch.size().max(0.0);
                        character_count += 1;
                        let is_bold = ch.flags().contains(TextCharFlags::BOLD)
                            || ch.font().is_some_and(|font| font.is_bold());
                        bold_count += usize::from(is_bold);
                    }
                }
                lines.push(text.trim_end_matches(['\r', '\n']).to_string());
            }

            if lines.iter().all(|line| line.trim().is_empty()) {
                continue;
            }
            blocks.push(TextFormatBlock {
                lines,
                average_font_size: if character_count == 0 {
                    0.0
                } else {
                    total_font_size / character_count as f32
                },
                bold_ratio: if character_count == 0 {
                    0.0
                } else {
                    bold_count as f32 / character_count as f32
                },
                layout: None,
            });
        }

        Ok(match format {
            PageTextFormat::Block => Self::format_block_markdown(&blocks),
            PageTextFormat::Semantic => Self::format_semantic_markdown(&blocks),
        })
    }

    fn format_block_markdown(blocks: &[TextFormatBlock]) -> String {
        blocks
            .iter()
            .map(|block| {
                block
                    .lines
                    .iter()
                    .map(|line| Self::escape_commonmark(line))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn format_semantic_markdown(blocks: &[TextFormatBlock]) -> String {
        let mut font_sizes = blocks
            .iter()
            .filter(|block| block.average_font_size.is_finite() && block.average_font_size > 0.0)
            .map(|block| block.average_font_size)
            .collect::<Vec<_>>();
        font_sizes.sort_by(f32::total_cmp);
        let baseline = font_sizes
            .get(font_sizes.len().saturating_sub(1) / 2)
            .copied()
            .unwrap_or(0.0);

        blocks
            .iter()
            .map(|block| {
                if let Some(list) = Self::semantic_list_markdown(&block.lines) {
                    return list;
                }
                if let Some(level) = Self::semantic_heading_level(block, baseline) {
                    let heading = block.lines[0].trim();
                    return format!("{} {}", "#".repeat(level), Self::escape_commonmark(heading));
                }

                block
                    .lines
                    .iter()
                    .map(|line| Self::escape_commonmark(line))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn semantic_heading_level(block: &TextFormatBlock, baseline: f32) -> Option<usize> {
        if baseline <= 0.0 || block.lines.len() != 1 {
            return None;
        }
        let text = block.lines[0].trim();
        if text.is_empty()
            || text.chars().count() > 120
            || text.ends_with(['.', ',', ';', ':', '?', '!'])
        {
            return None;
        }

        let relative_size = block.average_font_size / baseline;
        if relative_size >= 1.6 {
            Some(1)
        } else if relative_size >= 1.35 {
            Some(2)
        } else if relative_size >= 1.18 && block.bold_ratio >= 0.5 {
            Some(3)
        } else {
            None
        }
    }

    fn semantic_list_markdown(lines: &[String]) -> Option<String> {
        let non_empty = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        if non_empty.is_empty() {
            return None;
        }

        let mut items = Vec::with_capacity(non_empty.len());
        for line in non_empty {
            let trimmed = line.trim_start();
            if let Some(item) = Self::strip_unordered_list_marker(trimmed) {
                items.push(format!("- {}", Self::escape_commonmark(item)));
                continue;
            }
            if let Some((number, item)) = Self::strip_ordered_list_marker(trimmed) {
                items.push(format!("{number}. {}", Self::escape_commonmark(item)));
                continue;
            }
            return None;
        }
        Some(items.join("\n"))
    }

    fn strip_unordered_list_marker(line: &str) -> Option<&str> {
        ["•", "◦", "▪", "‣", "\u{f0a2}", "-", "*", "+"]
            .into_iter()
            .find_map(|marker| {
                line.strip_prefix(marker)
                    .and_then(|rest| rest.strip_prefix(char::is_whitespace))
                    .map(str::trim_start)
                    .filter(|item| !item.is_empty())
            })
    }

    fn strip_ordered_list_marker(line: &str) -> Option<(&str, &str)> {
        let digit_count = line.bytes().take_while(u8::is_ascii_digit).count();
        if digit_count == 0 {
            return None;
        }
        let number = &line[..digit_count];
        let rest = &line[digit_count..];
        let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
        let item = rest.strip_prefix(char::is_whitespace)?.trim_start();
        (!item.is_empty()).then_some((number, item))
    }

    fn escape_commonmark(text: &str) -> String {
        let mut escaped = String::with_capacity(text.len());
        for c in text.chars() {
            if matches!(
                c,
                '\\' | '`'
                    | '*'
                    | '_'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | '#'
                    | '+'
                    | '-'
                    | '.'
                    | '!'
                    | '|'
            ) {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        escaped
    }

    fn page_image_dimensions(
        page_width: f32,
        page_height: f32,
        max_dimension: u32,
    ) -> Result<(u32, u32)> {
        if !page_width.is_finite()
            || !page_height.is_finite()
            || page_width <= 0.0
            || page_height <= 0.0
            || max_dimension == 0
        {
            bail!("PDF page has invalid image dimensions");
        }
        let scale = max_dimension as f32 / page_width.max(page_height);
        let width = (page_width * scale)
            .round()
            .clamp(1.0, max_dimension as f32) as u32;
        let height = (page_height * scale)
            .round()
            .clamp(1.0, max_dimension as f32) as u32;
        Ok((width, height))
    }

    fn render_page_jpeg(
        display_list: &DisplayList,
        page_bounds: Rect,
        page_width: f32,
        page_height: f32,
        quality: PageImageQuality,
        rotation: DocumentRotation,
    ) -> Result<Vec<u8>> {
        let (display_width, display_height) =
            Self::rotated_dimensions(page_width, page_height, rotation);
        let (width, height) =
            Self::page_image_dimensions(display_width, display_height, quality.max_dimension())?;
        let mut pixmap = Pixmap::new_with_w_h(
            &Colorspace::device_rgb(),
            width as i32,
            height as i32,
            false,
        )
        .context("failed to allocate page image")?;
        pixmap.samples_mut().fill(255);

        let zoom_x = width as f32 / display_width;
        let zoom_y = height as f32 / display_height;
        let matrix = Self::render_matrix(
            page_bounds,
            rotation,
            zoom_x,
            zoom_y,
            mupdf::Point { x: 0.0, y: 0.0 },
        );
        {
            let device =
                Device::from_pixmap(&pixmap).context("failed to create page image device")?;
            display_list
                .run(&device, &matrix, Rect::INF)
                .context("failed to rasterize page image")?;
        }

        let samples = pixmap.samples();
        let expected_len = width as usize * height as usize * 3;
        if samples.len() != expected_len || pixmap.n() != 3 {
            bail!(
                "unexpected page image pixmap layout: {} samples, {} components",
                samples.len(),
                pixmap.n()
            );
        }
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, quality.jpeg_quality())
            .encode(samples, width, height, image::ExtendedColorType::Rgb8)
            .context("failed to encode page image as JPEG")?;
        Ok(bytes)
    }

    fn default_sidebar_widths() -> SidebarWidths {
        SidebarWidths {
            thumbnails: THUMBNAIL_SIDEBAR_WIDTH,
            toc: TOC_SIDEBAR_WIDTH,
            notes: NOTES_SIDEBAR_WIDTH,
            ai: AI_PANEL_WIDTH,
        }
    }

    fn sanitize_sidebar_widths(widths: SidebarWidths) -> SidebarWidths {
        let sanitize = |width: f32, fallback: f32| {
            if width.is_finite() {
                width.max(SIDEBAR_MIN_WIDTH)
            } else {
                fallback
            }
        };

        SidebarWidths {
            thumbnails: sanitize(widths.thumbnails, THUMBNAIL_SIDEBAR_WIDTH),
            toc: sanitize(widths.toc, TOC_SIDEBAR_WIDTH),
            notes: sanitize(widths.notes, NOTES_SIDEBAR_WIDTH),
            ai: sanitize(widths.ai, AI_PANEL_WIDTH),
        }
    }

    fn max_sidebar_width_for_window(window: &Window) -> f32 {
        (window.bounds().size.width.as_f32() * SIDEBAR_MAX_WIDTH_RATIO).max(SIDEBAR_MIN_WIDTH)
    }

    fn clamp_sidebar_width_for_window(width: f32, fallback: f32, window: &Window) -> f32 {
        let width = if width.is_finite() { width } else { fallback };
        width.clamp(
            SIDEBAR_MIN_WIDTH,
            Self::max_sidebar_width_for_window(window),
        )
    }

    fn clamp_ai_panel_width(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width =
            Self::clamp_sidebar_width_for_window(self.ai_panel_width, AI_PANEL_WIDTH, window);
        if (self.ai_panel_width - width).abs() <= f32::EPSILON {
            return;
        }

        self.ai_panel_width = width;
        if let Some(panel) = &self.ai_panel {
            panel.update(cx, |panel, cx| {
                panel.set_width(width);
                cx.notify();
            });
        }
        self.schedule_sidebar_width_save(cx);
    }

    fn close_left_sidebars_except(&mut self, keep_open: LeftSidebar) {
        if keep_open != LeftSidebar::Thumbnails {
            self.thumbnail_sidebar_open = false;
            self.thumbnail_sidebar_selecting = false;
            self.thumbnail_sidebar_selection.clear();
            self.thumbnail_sidebar_actions_position = None;
        }
        if keep_open != LeftSidebar::Toc {
            self.toc_sidebar_open = false;
        }
        if keep_open != LeftSidebar::Notes {
            self.notes_sidebar_open = false;
        }
    }

    pub fn prepare_open(
        path: impl AsRef<Path>,
        epub_preferences: EpubReaderPreferences,
        page_gap: f32,
    ) -> Result<PreparedPdfOpen> {
        let path = normalize_path(path.as_ref());
        let mut file_name: SharedString = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| crate::fl!("untitled"))
            .into();

        let document_snapshot = Arc::new(DocumentSnapshot::read(&path)?);
        let format = document_snapshot.format();
        let epub_preferences = epub_preferences.sanitized();
        let requested_layout = if format == DocumentFormat::Epub {
            DocumentLayout::Reflow(epub_preferences.layout_for_viewport(720.0, 960.0))
        } else {
            DocumentLayout::Fixed
        };
        let opened_document = document_snapshot.open_with_layout(requested_layout)?;
        let capabilities = opened_document.capabilities();
        let document_layout = opened_document.layout();
        if format == DocumentFormat::Epub
            && let Ok(title) = opened_document.inner().metadata(mupdf::MetadataName::Title)
            && !title.trim().is_empty()
        {
            file_name = title.into();
        }
        let document = opened_document.into_inner();
        let page_count = document.page_count()?;
        if page_count <= 0 {
            bail!("document has no readable content");
        }
        let outlines = match document.outlines() {
            Ok(outlines) => outlines,
            Err(error) => {
                eprintln!("failed to read document table of contents: {error:#}");
                Vec::new()
            }
        };
        let toc = Self::flatten_toc(outlines, page_count as usize);

        let page_count = page_count as usize;
        let loaded = Self::load_pages_from_document(&document, page_count, page_gap)?;

        Ok(PreparedPdfOpen {
            path,
            file_name,
            format,
            capabilities,
            document_layout,
            epub_preferences,
            document_snapshot,
            pages: loaded.pages,
            page_count,
            max_page_width: loaded.max_page_width.max(1.0),
            content_height_doc: loaded.content_height_doc,
            toc,
        })
    }

    pub fn from_prepared(
        prepared: PreparedPdfOpen,
        focus_handle: FocusHandle,
        behavior: BehaviorSettings,
        window_settings: WindowSettings,
        options: PdfOpenOptions,
    ) -> Result<Self> {
        let PreparedPdfOpen {
            path,
            file_name,
            format,
            capabilities,
            document_layout,
            epub_preferences,
            document_snapshot,
            pages,
            page_count,
            max_page_width,
            content_height_doc,
            toc,
        } = prepared;
        let rotation = if capabilities.reflow {
            DocumentRotation::default()
        } else {
            options.rotation
        };
        let page_image_bounds = (0..pages.len()).map(|_| None).collect();
        let sidebar_widths = options
            .sidebar_widths
            .map(Self::sanitize_sidebar_widths)
            .unwrap_or_else(Self::default_sidebar_widths);

        let mut view = Self {
            doc_id: next_document_id(),
            path,
            file_name,
            format,
            capabilities,
            document_layout,
            epub_preferences,
            epub_style_panel_position: None,
            document_snapshot,
            pages,
            page_count,
            max_page_width,
            content_height_doc,
            glyphs: Vec::new(),
            text_blocks: Vec::new(),
            text_lines: Vec::new(),
            page_line_ranges: vec![0..0; page_count],
            page_block_ranges: vec![0..0; page_count],
            translated_blocks: BTreeMap::new(),
            translated_text_layouts: HashMap::new(),
            translated_text_layout_frame: 0,
            view_language: PdfViewLanguage::Original,
            linked_viewport_generation: 0,
            linked_selection_generation: 0,
            suppress_linked_events: false,
            external_hovered_block: None,
            external_selected_block: None,
            hovered_text_block: None,
            translation_document_generation: 0,
            translation_document_task: None,
            links: Vec::new(),
            highlights: options.annotations.highlights,
            notes: options.annotations.notes,
            selected_highlight: None,
            selection: None,
            region_tool_active: false,
            region_drag: None,
            selected_region: None,
            translation_popup: None,
            pinned_translation: None,
            translation_popup_scroll: ScrollHandle::new(),
            pinned_translation_scroll: ScrollHandle::new(),
            target_language: options.target_language,
            translation_popup_language_dropdown: None,
            pinned_translation_language_dropdown: None,
            translation_request_generation: 0,
            document_translation_jobs: options.document_translation_jobs,
            pending_selection_anchor: None,
            dragging: false,
            hovered_link: None,
            hovered_note: None,
            hovered_note_position: None,
            pending_link: None,
            panning: None,
            page_image_bounds,
            focus_handle,
            pending_renders: Vec::new(),
            rotation,
            zoom: behavior.default_zoom,
            base_scale: 1.0,
            viewport_width: 0.0,
            viewport_height: 0.0,
            scroll_x_doc: 0.0,
            scroll_y_doc: 0.0,
            target_scroll_x_doc: 0.0,
            target_scroll_y_doc: 0.0,
            document_scroll: ManualScrollHandle::new(),
            ai_panel: None,
            quick_ask: None,
            quick_ask_visible: false,
            search_bar: None,
            toc_palette: None,
            notes_palette: None,
            history_palette: None,
            bookmark_palette: None,
            bookmark_prompt: None,
            go_to_page_prompt: None,
            note_editor: None,
            note_target: None,
            toc,
            toc_collapsed: HashSet::new(),
            toc_sidebar_open: false,
            toc_sidebar_width: sidebar_widths.toc,
            toc_sidebar_scroll: UniformListScrollHandle::new(),
            toc_sidebar_visible_entry_ixs: Vec::new(),
            toc_sidebar_entries_dirty: true,
            toc_sidebar_selected: None,
            notes_sidebar_open: false,
            notes_sidebar_width: sidebar_widths.notes,
            notes_sidebar_scroll: UniformListScrollHandle::new(),
            notes_sidebar_entries: Vec::new(),
            notes_sidebar_selected: None,
            thumbnail_sidebar_open: false,
            thumbnail_sidebar_width: sidebar_widths.thumbnails,
            thumbnail_sidebar_scroll: UniformListScrollHandle::new(),
            thumbnail_sidebar_current_page: 0,
            thumbnail_sidebar_selecting: false,
            thumbnail_sidebar_selection: BTreeSet::new(),
            thumbnail_sidebar_actions_position: None,
            ai_panel_width: sidebar_widths.ai,
            sidebar_width_save_task: None,
            grid_overview_open: false,
            grid_overview_scroll: UniformListScrollHandle::new(),
            grid_overview_zoom: GRID_OVERVIEW_DEFAULT_ZOOM,
            grid_overview_cols: GRID_OVERVIEW_DEFAULT_COLS,
            grid_overview_cursor: 0,
            grid_overview_viewport_width: 0.0,
            grid_overview_selecting: false,
            grid_overview_selection: BTreeSet::new(),
            grid_overview_actions_position: None,
            presentation_page: None,
            jump_back: Vec::new(),
            jump_forward: Vec::new(),
            position_from_search: false,
            transparency_enabled: window_settings.transparency_enabled_by_default(),
            window_settings,
            search: SearchState::default(),
            loading: false,
            loading_error: None,
            pending_initial_position: options.initial_position,
            pending_semantic_position: None,
            loading_task: None,
            reflow_resize_task: None,
            file_watch_task: None,
            reload_task: None,
            reload_coordinator: ReloadCoordinator::default(),
            previous_doc_id: None,
            previous_doc_evict_task: None,
            diff_flashes: Vec::new(),
            diff_pending_pages: BTreeSet::new(),
            diff_flash_clear_task: None,
            text_index_state: TextIndexState::Pending,
            text_indexing_task: None,
            position_save_task: None,
            context_menu_position: None,
            context_menu_page: None,
            context_menu_image: None,
            context_menu_highlight: None,
            context_menu_note: None,
            context_menu_region: false,
            context_menu_copy_submenu_open: false,
            document_edit_revision: DocumentEditRevision::default(),
            bookmarks_revision: 0,
            _image_clipboard: None,
        };
        if let Some(position) = view.pending_initial_position.take() {
            view.restore_position(position);
        }
        Ok(view)
    }

    pub fn register_cache_eviction(&mut self, cx: &mut Context<Self>) {
        let store = cx.global::<LibraryStore>();
        self.document_edit_revision = store.document_edit_revision(&self.path);
        self.bookmarks_revision = store.bookmarks_revision();

        cx.on_release(|this, cx| {
            if cx.has_global::<RenderCache>() {
                let mut doc_ids = Vec::with_capacity(2);
                doc_ids.push(this.doc_id);
                if let Some(previous_doc_id) = this.previous_doc_id
                    && previous_doc_id != this.doc_id
                {
                    doc_ids.push(previous_doc_id);
                }

                for doc_id in doc_ids {
                    let removed_images = cx.global_mut::<RenderCache>().evict_document(doc_id);
                    for image in removed_images {
                        cx.drop_image(image, None);
                    }
                }
            }
        })
        .detach();

        cx.observe_global::<SettingsStore>(|this, cx| {
            let (page_gap, window_settings) = {
                let store = cx.global::<SettingsStore>();
                (store.resolved_theme.page_gap, store.settings.window)
            };
            this.recompute_layout(page_gap);
            if this.window_settings != window_settings {
                this.transparency_enabled = window_settings.transparency_enabled_by_default();
                this.window_settings = window_settings;
            }
            cx.notify();
        })
        .detach();

        cx.observe_global::<LibraryStore>(|this, cx| {
            this.sync_document_edits_from_store(cx);
        })
        .detach();
    }

    fn initialize_translation_dropdowns(&mut self, cx: &mut Context<Self>) {
        if self.translation_popup_language_dropdown.is_some() {
            return;
        }
        let selected = self.target_language.clone();
        let popup = cx.new(|cx| {
            SearchableDropdown::new(
                target_language_items(&selected),
                selected.clone(),
                &crate::fl!("settings-search-languages-placeholder"),
                cx,
            )
            .width(180.0)
            .max_height(240.0)
        });
        let selected = self.target_language.clone();
        let pinned = cx.new(|cx| {
            SearchableDropdown::new(
                target_language_items(&selected),
                selected,
                &crate::fl!("settings-search-languages-placeholder"),
                cx,
            )
            .width(180.0)
            .max_height(240.0)
        });
        cx.subscribe(&popup, |this, _, event, cx| {
            let SearchableDropdownEvent::Selected(language) = event;
            this.change_target_language(language.clone(), cx);
        })
        .detach();
        cx.subscribe(&pinned, |this, _, event, cx| {
            let SearchableDropdownEvent::Selected(language) = event;
            this.change_target_language(language.clone(), cx);
        })
        .detach();
        cx.observe(&popup, |_, _, cx| cx.notify()).detach();
        cx.observe(&pinned, |_, _, cx| cx.notify()).detach();
        self.translation_popup_language_dropdown = Some(popup);
        self.pinned_translation_language_dropdown = Some(pinned);
    }

    pub fn start_loading(&mut self, cx: &mut Context<Self>) {
        self.initialize_translation_dropdowns(cx);
        if self.loading_task.is_some() {
            return;
        }

        self.start_file_watch(cx);
        if !self.loading {
            return;
        }

        let snapshot = self.document_snapshot.clone();
        let document_layout = self.document_layout;
        let page_count = self.page_count;
        let page_gap = cx.global::<SettingsStore>().resolved_theme.page_gap;

        self.loading_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let (loaded_page_count, loaded) =
                        Self::load_document_pages(&snapshot, document_layout, page_gap)
                            .map_err(|error| format!("{error:#}"))?;
                    if loaded_page_count != page_count {
                        return Err(format!(
                            "document page count changed while loading: expected {page_count}, got {loaded_page_count}"
                        ));
                    }
                    Ok(loaded)
                })
                .await;

            Self::finish_loading(this, cx, result);
        }));
    }

    fn file_signature(path: &Path) -> Option<FileSignature> {
        let metadata = fs::metadata(path).ok()?;
        Some(FileSignature {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }

    fn start_file_watch(&mut self, cx: &mut Context<Self>) {
        if self.file_watch_task.is_some() {
            return;
        }

        let path = self.path.clone();
        self.file_watch_task = Some(cx.spawn(async move |this, cx| {
            let mut last_signature = cx
                .background_spawn({
                    let path = path.clone();
                    async move { Self::file_signature(&path) }
                })
                .await;
            let mut pending_since: Option<Instant> = None;

            loop {
                cx.background_executor()
                    .timer(FILE_WATCH_POLL_INTERVAL)
                    .await;

                let signature = cx
                    .background_spawn({
                        let path = path.clone();
                        async move { Self::file_signature(&path) }
                    })
                    .await;

                let Some(signature) = signature else {
                    continue;
                };

                if last_signature != Some(signature) {
                    last_signature = Some(signature);
                    pending_since = Some(Instant::now());
                    continue;
                }

                if let Some(since) = pending_since
                    && since.elapsed() >= FILE_WATCH_STABLE_DELAY
                {
                    pending_since = None;
                    if this.update(cx, |this, cx| this.request_reload(cx)).is_err() {
                        break;
                    }
                }
            }
        }));
    }

    fn request_reload(&mut self, cx: &mut Context<Self>) {
        let Some(generation) = self.reload_coordinator.request() else {
            return;
        };
        self.start_reload(generation, cx);
    }

    fn start_reload(&mut self, generation: u64, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let document_layout = self.document_layout;
        let page_gap = cx.global::<SettingsStore>().resolved_theme.page_gap;

        self.reload_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn({
                    let path = path.clone();
                    async move {
                        Self::load_reload_data(&path, document_layout, page_gap)
                            .map_err(|error| format!("{error:#}"))
                    }
                })
                .await;

            Self::finish_reload(this, cx, generation, result);
        }));
    }

    fn load_reload_data(
        path: &Path,
        document_layout: DocumentLayout,
        page_gap: f32,
    ) -> Result<ReloadData> {
        let snapshot = Arc::new(DocumentSnapshot::read(path)?);
        let document = snapshot.open_with_layout(document_layout)?.into_inner();
        let page_count = document.page_count()?;
        if page_count <= 0 {
            bail!("document has no readable content");
        }
        let page_count = page_count as usize;
        let toc = match document.outlines() {
            Ok(outlines) => Self::flatten_toc(outlines, page_count),
            Err(error) => {
                eprintln!("failed to read PDF table of contents: {error:#}");
                Vec::new()
            }
        };
        let loaded = Self::load_pages_from_document(&document, page_count, page_gap)?;

        Ok(ReloadData {
            snapshot,
            page_count,
            loaded,
            toc,
        })
    }

    fn finish_reload(
        this: gpui::WeakEntity<Self>,
        cx: &mut AsyncApp,
        generation: u64,
        result: std::result::Result<ReloadData, String>,
    ) {
        let update_result = this.update(cx, |this, cx| {
            this.reload_task = None;
            match this.reload_coordinator.finish_reload(generation) {
                ReloadDisposition::Apply => match result {
                    Ok(reload) => {
                        if let Err(error) = this.apply_reloaded_document(reload, cx) {
                            eprintln!("failed to reload PDF: {error:#}");
                        }
                    }
                    Err(error) => eprintln!("failed to reload PDF: {error}"),
                },
                ReloadDisposition::DiscardAndStart(next_generation) => {
                    this.start_reload(next_generation, cx);
                }
                ReloadDisposition::Discard => {}
            }
        });
        if let Err(error) = update_result
            && error.to_string() != "entity released"
        {
            eprintln!("failed to update reloaded PDF: {error:#}");
        }
    }

    fn apply_reloaded_document(
        &mut self,
        reload: ReloadData,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let pending_position = self.pending_initial_position.take();
        let diff_pending_pages = if self.capabilities.reload_diff {
            self.page_image_bounds
                .iter()
                .enumerate()
                .filter_map(|(page_idx, bounds)| bounds.as_ref().map(|_| page_idx))
                .filter(|page_idx| *page_idx < reload.loaded.pages.len())
                .collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };

        self.evict_previous_document(cx);
        self.loading_task = None;
        self.text_indexing_task = None;
        self.pending_renders.clear();
        self.diff_flash_clear_task = None;
        self.diff_flashes.clear();

        let previous_doc_id = self.doc_id;
        self.previous_doc_id = Some(previous_doc_id);
        self.doc_id = next_document_id();
        self.schedule_previous_document_eviction(previous_doc_id, cx);

        self.document_snapshot = reload.snapshot;
        self.page_count = reload.page_count;
        self.toc = reload.toc;
        self.toc_collapsed
            .retain(|entry_ix| *entry_ix < self.toc.len());
        if self
            .toc_sidebar_selected
            .is_some_and(|entry_ix| entry_ix >= self.toc.len())
        {
            self.toc_sidebar_selected = None;
        }
        self.toc_sidebar_entries_dirty = true;
        self.pages = reload.loaded.pages;
        self.recompute_layout(cx.global::<SettingsStore>().resolved_theme.page_gap);
        self.glyphs.clear();
        self.text_blocks.clear();
        self.text_lines.clear();
        self.page_line_ranges = vec![0..0; self.pages.len()];
        self.page_block_ranges = vec![0..0; self.pages.len()];
        self.replace_translation_document();
        self.links.clear();
        self.search.matches.clear();
        self.search.active = None;
        self.selection = None;
        self.region_tool_active = false;
        self.region_drag = None;
        self.selected_region = None;
        self.context_menu_region = false;
        self.pending_selection_anchor = None;
        self.hovered_link = None;
        self.pending_link = None;
        self.page_image_bounds = (0..self.pages.len()).map(|_| None).collect();
        self.loading = false;
        self.loading_error = None;
        self.text_index_state = TextIndexState::Pending;
        self.search.cancel_reveal_after_indexing();
        self.diff_pending_pages = diff_pending_pages;
        self.grid_overview_cursor = self.grid_overview_cursor.min(self.last_page_index());
        self.thumbnail_sidebar_current_page = self
            .thumbnail_sidebar_current_page
            .min(self.last_page_index());
        self.grid_overview_selection
            .retain(|page_idx| *page_idx < self.pages.len());
        self.thumbnail_sidebar_selection
            .retain(|page_idx| *page_idx < self.pages.len());
        if self
            .context_menu_page
            .is_some_and(|page_idx| page_idx >= self.pages.len())
        {
            self.context_menu_page = None;
        }
        if let Some(position) = pending_position {
            self.restore_position(position);
        } else {
            self.clamp_scroll();
        }
        self.refresh_notes_sidebar_entries();
        self.sync_search_bar(cx);
        cx.notify();

        Ok(())
    }

    fn evict_cached_document(doc_id: u64, cx: &mut Context<Self>) {
        let removed_images = cx.global_mut::<RenderCache>().evict_document(doc_id);
        for image in removed_images {
            cx.drop_image(image, None);
        }
    }

    fn evict_previous_document(&mut self, cx: &mut Context<Self>) {
        self.previous_doc_evict_task = None;
        if let Some(previous_doc_id) = self.previous_doc_id.take() {
            Self::evict_cached_document(previous_doc_id, cx);
        }
    }

    fn schedule_previous_document_eviction(&mut self, doc_id: u64, cx: &mut Context<Self>) {
        self.previous_doc_evict_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(PREVIOUS_DOCUMENT_EVICT_DELAY)
                .await;

            let update_result = this.update(cx, |this, cx| {
                if this.previous_doc_id != Some(doc_id) {
                    return;
                }

                if !this.pending_renders.is_empty() {
                    this.previous_doc_evict_task = None;
                    this.schedule_previous_document_eviction(doc_id, cx);
                    return;
                }

                this.previous_doc_id = None;
                this.previous_doc_evict_task = None;
                Self::evict_cached_document(doc_id, cx);
                cx.notify();
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to evict previous PDF render cache: {error:#}");
            }
        }));
    }

    fn finish_loading(
        this: gpui::WeakEntity<Self>,
        cx: &mut AsyncApp,
        result: std::result::Result<LoadedPages, String>,
    ) {
        let update_result = this.update(cx, |this, cx| {
            this.loading_task = None;
            match result {
                Ok(loaded) => this.apply_loaded_pages(loaded, cx),
                Err(error) => this.apply_loading_error(error, cx),
            }
            if let Some(generation) = this.reload_coordinator.finish_initial_load() {
                this.start_reload(generation, cx);
            }
        });
        if let Err(error) = update_result
            && error.to_string() != "entity released"
        {
            eprintln!("failed to update loaded PDF: {error:#}");
        }
    }

    #[ztracing::instrument(skip_all)]
    fn load_document_pages(
        snapshot: &DocumentSnapshot,
        document_layout: DocumentLayout,
        page_gap: f32,
    ) -> Result<(usize, LoadedPages)> {
        let document = snapshot.open_with_layout(document_layout)?.into_inner();
        let page_count = document.page_count()?;
        if page_count <= 0 {
            bail!("document has no readable content");
        }
        let page_count = page_count as usize;
        let loaded = Self::load_pages_from_document(&document, page_count, page_gap)?;
        Ok((page_count, loaded))
    }

    fn load_pages_from_document(
        document: &mupdf::Document,
        page_count: usize,
        page_gap: f32,
    ) -> Result<LoadedPages> {
        let mut pages = Vec::with_capacity(page_count);
        let mut max_page_width = 0.0_f32;

        for page_idx in 0..page_count {
            let page = document.load_page(page_idx as i32)?;
            let page_bounds = page.bounds()?;
            let page_width = page_bounds.x1 - page_bounds.x0;
            let page_height = page_bounds.y1 - page_bounds.y0;

            pages.push(PdfPage {
                bounds: page_bounds,
                width: page_width,
                height: page_height,
                offset_y: 0.0,
                glyph_range: 0..0,
            });

            max_page_width = max_page_width.max(page_width);
        }

        let loaded = Self::merge_page_chunks(
            vec![PageChunk {
                start_page_idx: 0,
                pages,
                max_page_width,
            }],
            page_gap,
        )
        .map_err(anyhow::Error::msg)?;
        Ok(loaded)
    }

    #[cfg(test)]
    fn provisional_reflow_pages(
        page_count: usize,
        page_bounds: Rect,
        page_gap: f32,
    ) -> Result<LoadedPages> {
        let page_width = page_bounds.x1 - page_bounds.x0;
        let page_height = page_bounds.y1 - page_bounds.y0;
        if page_width <= 0.0 || page_height <= 0.0 {
            bail!("reflowable document page has invalid bounds");
        }
        let pages = (0..page_count)
            .map(|_| PdfPage {
                bounds: page_bounds,
                width: page_width,
                height: page_height,
                offset_y: 0.0,
                glyph_range: 0..0,
            })
            .collect();
        Self::merge_page_chunks(
            vec![PageChunk {
                start_page_idx: 0,
                pages,
                max_page_width: page_width,
            }],
            page_gap,
        )
        .map_err(anyhow::Error::msg)
    }

    fn merge_page_chunks(
        mut chunks: Vec<PageChunk>,
        page_gap: f32,
    ) -> std::result::Result<LoadedPages, String> {
        chunks.sort_by_key(|chunk| chunk.start_page_idx);

        let page_count = chunks.iter().map(|chunk| chunk.pages.len()).sum();
        let mut pages = Vec::with_capacity(page_count);
        let mut offset_y = 0.0;
        let mut max_page_width = 0.0_f32;

        for mut chunk in chunks {
            max_page_width = max_page_width.max(chunk.max_page_width);

            for page in &mut chunk.pages {
                page.offset_y = offset_y;
                offset_y += page.height + page_gap;
            }

            pages.extend(chunk.pages);
        }

        let content_height_doc = if pages.is_empty() {
            0.0
        } else {
            offset_y - page_gap
        };

        Ok(LoadedPages {
            pages,
            max_page_width,
            content_height_doc,
        })
    }

    fn apply_loaded_pages(&mut self, loaded: LoadedPages, cx: &mut Context<Self>) {
        self.pages = loaded.pages;
        self.recompute_layout(cx.global::<SettingsStore>().resolved_theme.page_gap);
        self.region_tool_active = false;
        self.region_drag = None;
        self.selected_region = None;
        self.glyphs.clear();
        self.text_blocks.clear();
        self.text_lines.clear();
        self.page_line_ranges = vec![0..0; self.pages.len()];
        self.page_block_ranges = vec![0..0; self.pages.len()];
        self.replace_translation_document();
        self.links.clear();
        self.search.matches.clear();
        self.search.active = None;
        self.selection = None;
        self.pending_selection_anchor = None;
        self.hovered_link = None;
        self.pending_link = None;
        self.page_image_bounds = (0..self.pages.len()).map(|_| None).collect();
        self.loading = false;
        self.loading_error = None;
        self.text_index_state = TextIndexState::Pending;
        if let Some(page_idx) = self.presentation_page.as_mut() {
            *page_idx = (*page_idx).min(self.pages.len().saturating_sub(1));
        }

        if let Some(position) = self.pending_initial_position.take() {
            self.restore_position(position);
        } else {
            self.clamp_scroll();
        }

        cx.notify();
    }

    fn apply_loading_error(&mut self, error: String, cx: &mut Context<Self>) {
        eprintln!("failed to load PDF pages: {error}");
        self.loading = false;
        self.text_index_state = TextIndexState::Failed;
        self.search.cancel_reveal_after_indexing();
        self.recompute_search();
        self.loading_error = Some(error);
        self.sync_search_bar(cx);
        cx.notify();
    }

    fn start_text_indexing(&mut self, cx: &mut Context<Self>) {
        if self.text_indexing_task.is_some()
            || self.pages.is_empty()
            || !self.text_index_state.begin()
        {
            return;
        }

        self.sync_search_bar(cx);

        let snapshot = self.document_snapshot.clone();
        let document_layout = self.document_layout;
        let page_count = self.pages.len();
        let page_bounds: Vec<Rect> = self.pages.iter().map(|page| page.bounds).collect();

        self.text_indexing_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;

            let result = cx
                .background_spawn(async move {
                    let chunk = Self::load_page_text_chunk(
                        &snapshot,
                        document_layout,
                        0,
                        page_count,
                        page_count,
                        page_bounds,
                    )
                    .map_err(|error| format!("{error:#}"))?;
                    Self::merge_text_chunks(vec![chunk], page_count)
                })
                .await;
            Self::finish_text_indexing(this, cx, result);
        }));
    }

    #[ztracing::instrument(skip_all, fields(start_page_idx, end_page_idx, page_count))]
    fn load_page_text_chunk(
        snapshot: &DocumentSnapshot,
        document_layout: DocumentLayout,
        start_page_idx: usize,
        end_page_idx: usize,
        page_count: usize,
        page_bounds: Vec<Rect>,
    ) -> Result<TextChunk> {
        let document = snapshot.open_with_layout(document_layout)?.into_inner();
        let mut glyphs = Vec::new();
        let mut text_lines = Vec::new();
        let mut links = Vec::new();
        let mut page_glyph_ranges = Vec::with_capacity(end_page_idx.saturating_sub(start_page_idx));
        let mut page_line_ranges = Vec::with_capacity(end_page_idx.saturating_sub(start_page_idx));
        let mut next_line_idx = 0;
        let mut next_block_idx = 0;

        for page_idx in start_page_idx..end_page_idx {
            let page = document.load_page(page_idx as i32)?;
            let page_bounds = page_bounds[page_idx - start_page_idx];
            let display_list = DisplayList::new(page_bounds)?;
            let device = Device::from_display_list(&display_list)?;
            page.run(&device, &Matrix::IDENTITY)?;

            let explicit_links_start = links.len();
            match Self::extract_explicit_links(&page, page_idx, page_count) {
                Ok(page_links) => links.extend(page_links),
                Err(error) => {
                    eprintln!(
                        "failed to read PDF links on page {}: {error:#}",
                        page_idx + 1
                    );
                }
            }

            let line_start = text_lines.len();
            let glyph_range = Self::extract_glyphs(
                &display_list,
                page_idx,
                &mut glyphs,
                &mut text_lines,
                &mut next_line_idx,
                &mut next_block_idx,
            )?;

            let detected_links = Self::detect_text_url_links(
                page_idx,
                &glyphs,
                glyph_range.clone(),
                &links[explicit_links_start..],
            );
            links.extend(detected_links);
            page_glyph_ranges.push(glyph_range);
            page_line_ranges.push(line_start..text_lines.len());
        }

        let text_blocks = Self::build_text_blocks(&glyphs);
        let mut page_block_ranges = Vec::with_capacity(page_glyph_ranges.len());
        let mut block_start = 0;
        for page_idx in start_page_idx..end_page_idx {
            let page_start = block_start;
            while text_blocks
                .get(block_start)
                .is_some_and(|block| block.page_idx == page_idx)
            {
                block_start += 1;
            }
            page_block_ranges.push(page_start..block_start);
        }

        Ok(TextChunk {
            start_page_idx,
            glyphs,
            text_blocks,
            text_lines,
            links,
            page_glyph_ranges,
            page_line_ranges,
            page_block_ranges,
            line_count: next_line_idx,
            block_count: next_block_idx,
        })
    }

    fn merge_text_chunks(
        mut chunks: Vec<TextChunk>,
        page_count: usize,
    ) -> std::result::Result<LoadedText, String> {
        chunks.sort_by_key(|chunk| chunk.start_page_idx);

        let glyph_count = chunks.iter().map(|chunk| chunk.glyphs.len()).sum();
        let block_count = chunks.iter().map(|chunk| chunk.text_blocks.len()).sum();
        let line_count = chunks.iter().map(|chunk| chunk.text_lines.len()).sum();
        let link_count = chunks.iter().map(|chunk| chunk.links.len()).sum();
        let mut glyphs = Vec::with_capacity(glyph_count);
        let mut text_blocks = Vec::with_capacity(block_count);
        let mut text_lines = Vec::with_capacity(line_count);
        let mut links = Vec::with_capacity(link_count);
        let mut page_glyph_ranges = vec![0..0; page_count];
        let mut page_line_ranges = vec![0..0; page_count];
        let mut page_block_ranges = vec![0..0; page_count];
        let mut line_offset = 0_u32;
        let mut block_offset = 0_u32;

        for mut chunk in chunks {
            let glyph_offset = glyphs.len();

            for glyph in &mut chunk.glyphs {
                glyph.line_idx = glyph
                    .line_idx
                    .checked_add(line_offset)
                    .ok_or_else(|| "PDF contains too many text lines".to_string())?;
                glyph.block_idx = glyph
                    .block_idx
                    .checked_add(block_offset)
                    .ok_or_else(|| "PDF contains too many text blocks".to_string())?;
            }

            for block in &mut chunk.text_blocks {
                block.id = block
                    .id
                    .checked_add(block_offset)
                    .ok_or_else(|| "PDF contains too many text blocks".to_string())?;
                block.glyph_range = (block.glyph_range.start + glyph_offset)
                    ..(block.glyph_range.end + glyph_offset);
            }

            for line in &mut chunk.text_lines {
                line.glyph_range =
                    (line.glyph_range.start + glyph_offset)..(line.glyph_range.end + glyph_offset);
            }

            for (offset, range) in chunk.page_glyph_ranges.into_iter().enumerate() {
                let page_idx = chunk.start_page_idx + offset;
                if page_idx >= page_count {
                    return Err("PDF text chunk exceeded page count".to_string());
                }

                page_glyph_ranges[page_idx] =
                    (range.start + glyph_offset)..(range.end + glyph_offset);
            }

            let text_line_offset = text_lines.len();
            for (offset, range) in chunk.page_line_ranges.into_iter().enumerate() {
                let page_idx = chunk.start_page_idx + offset;
                if page_idx >= page_count {
                    return Err("PDF text chunk exceeded page count".to_string());
                }

                page_line_ranges[page_idx] =
                    (range.start + text_line_offset)..(range.end + text_line_offset);
            }

            let text_block_offset = text_blocks.len();
            for (offset, range) in chunk.page_block_ranges.into_iter().enumerate() {
                let page_idx = chunk.start_page_idx + offset;
                if page_idx >= page_count {
                    return Err("PDF text chunk exceeded page count".to_string());
                }

                page_block_ranges[page_idx] =
                    (range.start + text_block_offset)..(range.end + text_block_offset);
            }

            line_offset = line_offset
                .checked_add(chunk.line_count)
                .ok_or_else(|| "PDF contains too many text lines".to_string())?;
            block_offset = block_offset
                .checked_add(chunk.block_count)
                .ok_or_else(|| "PDF contains too many text blocks".to_string())?;

            glyphs.extend(chunk.glyphs);
            text_blocks.extend(chunk.text_blocks);
            text_lines.extend(chunk.text_lines);
            links.extend(chunk.links);
        }

        Ok(LoadedText {
            glyphs,
            text_blocks,
            text_lines,
            links,
            page_glyph_ranges,
            page_line_ranges,
            page_block_ranges,
        })
    }

    fn finish_text_indexing(
        this: gpui::WeakEntity<Self>,
        cx: &mut AsyncApp,
        result: std::result::Result<LoadedText, String>,
    ) {
        let update_result = this.update(cx, |this, cx| {
            this.text_indexing_task = None;
            match result {
                Ok(loaded) => this.apply_text_data(loaded, cx),
                Err(error) => this.apply_text_indexing_error(error, cx),
            }
        });
        if let Err(error) = update_result
            && error.to_string() != "entity released"
        {
            eprintln!("failed to update indexed PDF text: {error:#}");
        }
    }

    fn apply_text_data(&mut self, loaded: LoadedText, cx: &mut Context<Self>) {
        self.glyphs = loaded.glyphs;
        self.text_blocks = loaded.text_blocks;
        self.text_lines = loaded.text_lines;
        self.page_line_ranges = loaded.page_line_ranges;
        self.page_block_ranges = loaded.page_block_ranges;
        self.invalidate_translation_document();
        self.links = loaded.links;
        self.text_index_state.settle(true);
        self.hovered_link = None;
        self.pending_link = None;
        self.pending_selection_anchor = None;

        for (page, glyph_range) in self.pages.iter_mut().zip(loaded.page_glyph_ranges) {
            page.glyph_range = glyph_range;
        }

        self.refresh_semantic_annotations();
        self.refresh_notes_sidebar_entries();
        if let Some(position) = self.pending_semantic_position.take() {
            self.restore_position(position);
        }
        if self.capabilities.reflow {
            self.record_current_history_position(cx);
            Self::persist_history_entry(&self.path, cx);
        }
        self.recompute_search();
        self.search.active = self.nearest_search_match_index();
        let reveal_search_match = self.search.take_reveal_after_indexing();
        self.sync_search_bar(cx);

        if reveal_search_match && self.search.active.is_some() {
            self.record_search_jump_origin();
            self.scroll_to_active_search_match(cx);
        }

        if self.view_language == PdfViewLanguage::Translated {
            self.request_document_translation(cx);
        }
        cx.notify();
    }

    fn invalidate_translation_document(&mut self) {
        self.translation_document_generation = self.translation_document_generation.wrapping_add(1);
        self.translation_document_task = None;
        self.translated_blocks.clear();
        self.translated_text_layouts.clear();
        self.hovered_text_block = None;
        self.external_hovered_block = None;
        self.external_selected_block = None;
    }

    fn replace_translation_document(&mut self) {
        self.invalidate_translation_document();
        self.translation_request_generation = self.translation_request_generation.wrapping_add(1);
        self.reset_translation_scroll(false);
        self.reset_translation_scroll(true);
        reset_translation_cards_for_document_replacement(
            &mut self.translation_popup,
            &mut self.pinned_translation,
            self.translation_request_generation,
        );
    }

    #[ztracing::instrument(skip_all, fields(glyph_count = glyphs.len()))]
    fn build_text_blocks(glyphs: &[Glyph]) -> Vec<TextBlock> {
        let mut blocks = Vec::new();
        let mut start = 0;
        while start < glyphs.len() {
            let first = &glyphs[start];
            let block_id = first.block_idx;
            let mut end = start + 1;
            while end < glyphs.len() && glyphs[end].block_idx == block_id {
                end += 1;
            }
            let block_glyphs = &glyphs[start..end];
            let mut bounds = Self::glyph_line_rect(&block_glyphs[0]);
            let mut font_size = 0.0;
            let mut colors = BTreeMap::<u32, usize>::new();
            let mut text = String::new();
            for (offset, glyph) in block_glyphs.iter().enumerate() {
                if offset > 0 {
                    let previous = &block_glyphs[offset - 1];
                    if glyph.line_idx != previous.line_idx {
                        text.push('\n');
                    } else if previous.inferred_space_after {
                        text.push(' ');
                    }
                }
                bounds = Self::union_rect(bounds, Self::glyph_line_rect(glyph));
                font_size += glyph.font_size;
                *colors.entry(glyph.color).or_default() += 1;
                text.push(glyph.c);
            }
            let color = colors
                .into_iter()
                .max_by_key(|(_, count)| *count)
                .map(|(color, _)| color)
                .unwrap_or(0);
            blocks.push(TextBlock {
                id: block_id,
                page_idx: first.page_idx,
                glyph_range: start..end,
                bounds,
                text,
                font_size: (font_size / block_glyphs.len() as f32).max(6.0),
                color,
                horizontal: true,
            });
            start = end;
        }
        blocks
    }

    fn apply_text_indexing_error(&mut self, error: String, cx: &mut Context<Self>) {
        eprintln!("failed to index PDF text: {error}");
        self.text_index_state.settle(false);
        self.search.cancel_reveal_after_indexing();
        self.recompute_search();
        self.sync_search_bar(cx);
        cx.notify();
    }

    fn flatten_toc(outlines: Vec<mupdf::Outline>, page_count: usize) -> Vec<TocEntry> {
        let mut entries = Vec::new();
        for outline in outlines {
            Self::push_toc_outline(outline, 0, page_count, &mut entries);
        }
        entries
    }

    fn push_toc_outline(
        outline: mupdf::Outline,
        depth: usize,
        page_count: usize,
        entries: &mut Vec<TocEntry>,
    ) {
        let mupdf::Outline {
            title, dest, down, ..
        } = outline;
        let has_children = !down.is_empty();
        let (page_idx, target_top, target_left, navigable) = match dest {
            Some(dest) => {
                let page_idx = dest.loc.page_number as usize;
                let navigable = page_idx < page_count;
                let page_idx = page_idx.min(page_count.saturating_sub(1));
                let (target_top, target_left) = Self::destination_coordinates(dest.kind);
                (page_idx, target_top, target_left, navigable)
            }
            None => (0, None, None, false),
        };

        entries.push(TocEntry {
            title: title.into(),
            depth,
            has_children,
            page_idx,
            target_top,
            target_left,
            navigable,
        });

        for child in down {
            Self::push_toc_outline(child, depth + 1, page_count, entries);
        }
    }

    fn destination_coordinates(kind: DestinationKind) -> (Option<f32>, Option<f32>) {
        match kind {
            DestinationKind::XYZ { left, top, .. } => (top, left),
            DestinationKind::FitH { top } | DestinationKind::FitBH { top } => (top, None),
            DestinationKind::FitV { left } | DestinationKind::FitBV { left } => (None, left),
            DestinationKind::FitR { left, top, .. } => (Some(top), Some(left)),
            DestinationKind::Fit | DestinationKind::FitB => (None, None),
        }
    }

    fn extract_explicit_links(
        page: &mupdf::Page,
        page_idx: usize,
        page_count: usize,
    ) -> Result<Vec<PdfLink>> {
        let mut links = Vec::new();

        for link in page.links()? {
            let Some(bounds) = Self::normalized_rect(link.bounds) else {
                continue;
            };

            let target = match link.dest {
                Some(dest) => {
                    let target_page = dest.loc.page_number as usize;
                    if target_page >= page_count {
                        continue;
                    }

                    let (target_top, target_left) = Self::destination_coordinates(dest.kind);
                    LinkTarget::Internal {
                        page_idx: target_page,
                        target_top,
                        target_left,
                    }
                }
                None => {
                    let Some(uri) = Self::normalize_external_uri(&link.uri) else {
                        continue;
                    };
                    LinkTarget::External(uri)
                }
            };

            links.push(PdfLink {
                page_idx,
                rects: vec![bounds],
                target,
            });
        }

        Ok(links)
    }

    fn detect_text_url_links(
        page_idx: usize,
        glyphs: &[Glyph],
        glyph_range: Range<usize>,
        explicit_links: &[PdfLink],
    ) -> Vec<PdfLink> {
        let mut chars = Vec::new();
        let mut glyph_indices = Vec::new();

        for glyph_ix in glyph_range {
            let c = glyphs[glyph_ix].c;
            if c.is_control() {
                continue;
            }
            chars.push(c);
            glyph_indices.push(glyph_ix);
        }

        let mut links = Vec::new();
        let mut ix = 0;
        while ix < chars.len() {
            if !Self::url_start_boundary(&chars, ix) {
                ix += 1;
                continue;
            }

            let Some(prefix_len) = Self::detected_url_prefix_len(&chars, ix) else {
                ix += 1;
                continue;
            };

            let mut end = ix + prefix_len;
            while end < chars.len() && Self::is_url_body_char(chars[end]) {
                end += 1;
            }

            let trimmed_end = Self::trim_url_end(&chars, ix, end);
            if trimmed_end <= ix + prefix_len {
                ix += 1;
                continue;
            }

            let raw_url = chars[ix..trimmed_end].iter().collect::<String>();
            let Some(url) = Self::normalize_detected_url(&raw_url) else {
                ix = end.max(ix + 1);
                continue;
            };

            let rects = Self::link_rects_for_glyph_indices(glyphs, &glyph_indices[ix..trimmed_end]);
            if !rects.is_empty()
                && !Self::link_rects_overlap_existing(page_idx, &rects, explicit_links)
            {
                links.push(PdfLink {
                    page_idx,
                    rects,
                    target: LinkTarget::External(url),
                });
            }

            ix = end.max(ix + 1);
        }

        links
    }

    fn detected_url_prefix_len(chars: &[char], ix: usize) -> Option<usize> {
        for prefix in ["https://", "http://", "www."] {
            if Self::starts_with_ascii_case_insensitive(chars, ix, prefix) {
                return Some(prefix.len());
            }
        }

        None
    }

    fn starts_with_ascii_case_insensitive(chars: &[char], ix: usize, pattern: &str) -> bool {
        pattern.chars().enumerate().all(|(offset, expected)| {
            chars
                .get(ix + offset)
                .is_some_and(|actual| actual.eq_ignore_ascii_case(&expected))
        })
    }

    fn url_start_boundary(chars: &[char], ix: usize) -> bool {
        ix == 0 || Self::is_url_start_boundary_char(chars[ix - 1])
    }

    fn is_url_start_boundary_char(c: char) -> bool {
        c.is_whitespace()
            || c.is_control()
            || (!c.is_ascii_alphanumeric()
                && !matches!(
                    c,
                    '%' | '/' | '.' | '-' | '_' | '~' | '@' | '&' | '=' | '+' | '#'
                ))
    }

    fn is_url_body_char(c: char) -> bool {
        !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>' | '"' | '“' | '”' | '`')
    }

    fn trim_url_end(chars: &[char], start: usize, mut end: usize) -> usize {
        while end > start && Self::should_trim_url_trailing_char(chars, start, end) {
            end -= 1;
        }

        end
    }

    fn should_trim_url_trailing_char(chars: &[char], start: usize, end: usize) -> bool {
        match chars[end - 1] {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '’' | '”' => true,
            ')' => Self::has_unmatched_trailing_closer(chars, start, end, '(', ')'),
            ']' => Self::has_unmatched_trailing_closer(chars, start, end, '[', ']'),
            '}' => Self::has_unmatched_trailing_closer(chars, start, end, '{', '}'),
            _ => false,
        }
    }

    fn has_unmatched_trailing_closer(
        chars: &[char],
        start: usize,
        end: usize,
        open: char,
        close: char,
    ) -> bool {
        let mut opens = 0;
        let mut closes = 0;
        for c in &chars[start..end] {
            if *c == open {
                opens += 1;
            } else if *c == close {
                closes += 1;
            }
        }

        closes > opens
    }

    fn normalize_detected_url(url: &str) -> Option<String> {
        if Self::starts_with_ascii_case_insensitive(&url.chars().collect::<Vec<_>>(), 0, "www.") {
            Some(format!("https://{url}"))
        } else {
            Self::normalize_external_uri(url)
        }
    }

    fn normalize_external_uri(uri: &str) -> Option<String> {
        let uri = uri.trim();
        if uri.is_empty() {
            return None;
        }

        let lower = uri.to_ascii_lowercase();
        if lower.starts_with("www.") {
            return Some(format!("https://{uri}"));
        }

        let (scheme, _) = uri.split_once(':')?;
        matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto" | "ftp" | "ftps" | "tel"
        )
        .then(|| uri.to_string())
    }

    fn link_rects_for_glyph_indices(glyphs: &[Glyph], glyph_indices: &[usize]) -> Vec<Rect> {
        let mut rects = Vec::new();
        let mut current_line = None;
        let mut current_rect = None;

        for &glyph_ix in glyph_indices {
            let glyph = &glyphs[glyph_ix];
            let Some(rect) = Self::normalized_rect(Self::quad_rect(&glyph.quad)) else {
                continue;
            };

            match (current_line, current_rect) {
                (Some(line), Some(existing)) if line == glyph.line_idx => {
                    current_rect = Self::normalized_rect(Self::union_rect(existing, rect));
                }
                (Some(_), Some(existing)) => {
                    rects.push(existing);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
                _ => {
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
            }
        }

        if let Some(rect) = current_rect {
            rects.push(rect);
        }

        rects
    }

    fn link_rects_overlap_existing(
        page_idx: usize,
        rects: &[Rect],
        existing_links: &[PdfLink],
    ) -> bool {
        existing_links
            .iter()
            .filter(|link| link.page_idx == page_idx)
            .any(|link| {
                rects.iter().any(|rect| {
                    link.rects
                        .iter()
                        .any(|existing| Self::rects_intersect(*rect, *existing))
                })
            })
    }

    #[ztracing::instrument(skip_all, fields(page_idx))]
    fn extract_glyphs(
        display_list: &DisplayList,
        page_idx: usize,
        glyphs: &mut Vec<Glyph>,
        text_lines: &mut Vec<TextLine>,
        next_line_idx: &mut u32,
        next_block_idx: &mut u32,
    ) -> Result<Range<usize>> {
        let start = glyphs.len();
        let text_page = display_list
            .to_text_page(TextPageFlags::PRESERVE_WHITESPACE | TextPageFlags::ACCURATE_BBOXES)?;

        for block in text_page.blocks() {
            if block.r#type() != TextBlockType::Text {
                continue;
            }

            let block_idx = *next_block_idx;
            *next_block_idx = next_block_idx.saturating_add(1);

            for line in block.lines() {
                let line_idx = *next_line_idx;
                let line_rect = line.bounds();
                let line_start = glyphs.len();
                for ch in line.chars() {
                    if let Some(c) = ch.char() {
                        glyphs.push(Glyph {
                            c,
                            quad: ch.quad(),
                            line_rect,
                            page_idx,
                            line_idx,
                            block_idx,
                            font_size: ch.size(),
                            color: ch.argb() & 0x00ff_ffff,
                            is_bold: ch.flags().contains(TextCharFlags::BOLD)
                                || ch.font().is_some_and(|font| font.is_bold()),
                            inferred_space_after: false,
                        });
                    }
                }

                let line_end = glyphs.len();
                if line_start < line_end {
                    Self::mark_inferred_spaces(&mut glyphs[line_start..line_end]);
                    text_lines.push(TextLine {
                        glyph_range: line_start..line_end,
                        bounds: line_rect,
                    });
                }
                *next_line_idx = next_line_idx.saturating_add(1);
            }
        }

        Ok(start..glyphs.len())
    }

    fn diff_flash_overlays(&self, now: Instant) -> Vec<DiffFlashOverlay> {
        let duration = DIFF_FLASH_DURATION.as_secs_f32().max(f32::EPSILON);
        self.diff_flashes
            .iter()
            .filter_map(|flash| {
                if flash.page_idx >= self.pages.len() {
                    return None;
                }

                let elapsed = now.saturating_duration_since(flash.started_at);
                if elapsed >= DIFF_FLASH_DURATION {
                    return None;
                }

                let image_bounds = *self
                    .page_image_bounds
                    .get(flash.page_idx)
                    .and_then(Option::as_ref)?;
                let progress = (elapsed.as_secs_f32() / duration).clamp(0.0, 1.0);
                let alpha = (0x66 as f32 * (1.0 - progress))
                    .round()
                    .clamp(0.0, 0x66 as f32) as u8;

                Some(DiffFlashOverlay {
                    bounds: self.pdf_rect_to_screen(flash.page_idx, flash.rect, image_bounds),
                    alpha,
                })
            })
            .collect()
    }

    fn assemble_paint_data(&mut self, pages: Vec<PaintPage>, window: &mut Window) -> PaintData {
        let (highlight_overlays, selected_highlight_borders) = self.highlight_overlays();
        let note_underlines = self.note_underlines();
        let selection_highlights = self.selection_highlights();
        let (search_highlights, active_search_highlights) = self.search_highlights();
        let link_highlights = self.link_highlights();
        let linked_hover_highlights = self.external_block_highlights(self.external_hovered_block);
        let linked_selection_highlights =
            self.external_block_highlights(self.external_selected_block);
        let now = Instant::now();
        self.diff_flashes
            .retain(|flash| now.saturating_duration_since(flash.started_at) < DIFF_FLASH_DURATION);
        let diff_flash_overlays = self.diff_flash_overlays(now);
        let translated_text_overlays = self.translated_text_overlays(window);
        let region_overlays = self
            .active_region_reference()
            .map_or_else(Vec::new, |reference| self.region_overlay_bounds(&reference));

        PaintData {
            pages,
            translated_text_overlays,
            diff_flash_overlays,
            highlight_overlays,
            selected_highlight_borders,
            note_underlines,
            link_highlights,
            linked_hover_highlights,
            linked_selection_highlights,
            selection_highlights,
            search_highlights,
            active_search_highlights,
            region_overlays,
        }
    }

    fn translated_text_overlays(&mut self, window: &mut Window) -> Vec<TranslatedTextOverlay> {
        if self.view_language != PdfViewLanguage::Translated {
            return Vec::new();
        }
        self.translated_text_layout_frame = self.translated_text_layout_frame.wrapping_add(1);
        let frame = self.translated_text_layout_frame;
        let font = window.text_style().font();
        let translated_blocks = self
            .page_block_ranges
            .iter()
            .enumerate()
            .filter(|(page_idx, _)| {
                self.page_image_bounds
                    .get(*page_idx)
                    .is_some_and(Option::is_some)
            })
            .flat_map(|(_, range)| &self.text_blocks[range.clone()])
            .filter_map(|block| self.translated_blocks.get(&block.id))
            .cloned()
            .collect::<Vec<_>>();
        let overlays = translated_blocks
            .iter()
            .filter_map(|block| {
                let image_bounds = *self
                    .page_image_bounds
                    .get(block.source.page_idx)
                    .and_then(Option::as_ref)?;
                let bounds = self.pdf_rect_to_screen(
                    block.source.page_idx,
                    block.source.bounds,
                    image_bounds,
                );
                let scale = bounds.size.height.as_f32()
                    / (block.source.bounds.y1 - block.source.bounds.y0).max(1.0);
                let padding = px(1.0);
                let font_size = px((block.source.font_size * scale * 0.92).max(6.0));
                let key = TranslatedTextLayoutKey {
                    block_id: block.source.id,
                    text: block.text.clone().into(),
                    font: font.clone(),
                    color: block.source.color,
                    font_size_bits: font_size.as_f32().to_bits(),
                    width_bits: bounds.size.width.as_f32().to_bits(),
                    height_bits: bounds.size.height.as_f32().to_bits(),
                };
                let prepared = if let Some(cached) = self.translated_text_layouts.get_mut(&key) {
                    cached.last_used_frame = frame;
                    cached.prepared.clone()
                } else {
                    let prepared = Arc::new(Self::prepare_translated_text(
                        block.text.clone().into(),
                        font.clone(),
                        block.source.color,
                        font_size,
                        bounds,
                        window,
                    )?);
                    self.translated_text_layouts.insert(
                        key,
                        CachedTranslatedTextLayout {
                            prepared: prepared.clone(),
                            last_used_frame: frame,
                        },
                    );
                    prepared
                };
                Some(TranslatedTextOverlay {
                    mask: Bounds::new(
                        point(bounds.left() - padding, bounds.top() - padding),
                        size(
                            bounds.size.width + padding * 2.0,
                            bounds.size.height + padding * 2.0,
                        ),
                    ),
                    bounds,
                    prepared,
                })
            })
            .collect::<Vec<_>>();
        self.prune_translated_text_layouts(frame);
        overlays
    }

    fn prepare_translated_text(
        text: SharedString,
        font: Font,
        color: u32,
        requested_font_size: Pixels,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> Option<PreparedTranslatedText> {
        let mut font_size = requested_font_size;
        for _ in 0..12 {
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: rgb(color).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let lines = match window.text_system().shape_text(
                text.clone(),
                font_size,
                &[run],
                Some(bounds.size.width),
                None,
            ) {
                Ok(lines) => lines,
                Err(error) => {
                    eprintln!("failed to shape translated PDF text: {error:#}");
                    return None;
                }
            };
            let line_height = font_size * 1.15;
            let height: Pixels = lines.iter().map(|line| line.size(line_height).height).sum();
            if height <= bounds.size.height || font_size <= px(6.0) {
                return Some(PreparedTranslatedText {
                    lines: lines.into_iter().map(Arc::new).collect(),
                    line_height,
                });
            }
            font_size = (font_size * 0.9).max(px(6.0));
        }
        None
    }

    fn prune_translated_text_layouts(&mut self, frame: u64) {
        self.translated_text_layouts.retain(|_, cached| {
            cached.last_used_frame == frame
                || frame.saturating_sub(cached.last_used_frame) <= TRANSLATED_LAYOUT_KEEP_FRAMES
        });
        if self.translated_text_layouts.len() <= MAX_CACHED_TRANSLATED_LAYOUTS {
            return;
        }

        let mut candidates = self
            .translated_text_layouts
            .iter()
            .map(|(key, cached)| (key.clone(), cached.last_used_frame))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(_, last_used_frame)| *last_used_frame);
        let remove_count = self
            .translated_text_layouts
            .len()
            .saturating_sub(MAX_CACHED_TRANSLATED_LAYOUTS);
        for (key, _) in candidates.into_iter().take(remove_count) {
            self.translated_text_layouts.remove(&key);
        }
    }

    fn presentation_scale_for_page(&self, page_idx: usize) -> f32 {
        if self.pages.get(page_idx).is_none() {
            return self.effective_scale();
        }

        let (page_width, page_height) = self.page_dimensions(page_idx);
        let width_scale = self.viewport_width / page_width.max(1.0);
        let height_scale = self.viewport_height / page_height.max(1.0);
        width_scale.min(height_scale).max(0.001)
    }

    fn presentation_image_bounds_for_page(
        &self,
        page_idx: usize,
        bounds: Bounds<Pixels>,
        scale: f32,
    ) -> Bounds<Pixels> {
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let image_width = px(page_width * scale);
        let image_height = px(page_height * scale);
        let origin = point(
            bounds.origin.x + (bounds.size.width - image_width) / 2.0,
            bounds.origin.y + (bounds.size.height - image_height) / 2.0,
        );

        Bounds::new(origin, size(image_width, image_height))
    }

    fn presentation_paint_data(
        &mut self,
        bounds: Bounds<Pixels>,
        scale_factor: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaintData {
        let page_idx = self
            .presentation_page
            .unwrap_or_else(|| self.page_index_for_scroll_y(self.scroll_y_doc))
            .min(self.last_page_index());
        self.presentation_page = Some(page_idx);
        self.page_image_bounds.clear();
        self.page_image_bounds
            .resize_with(self.pages.len(), || None);

        let scale = self.presentation_scale_for_page(page_idx);
        let image_bounds = self.presentation_image_bounds_for_page(page_idx, bounds, scale);
        let image_bounds = Self::pixel_aligned_image_bounds(image_bounds, scale_factor);
        let key = self.render_key(page_idx, image_bounds, scale_factor);
        self.page_image_bounds[page_idx] = Some(image_bounds);

        let doc_id = self.doc_id;
        cx.global_mut::<RenderCache>()
            .set_protected_pages([(doc_id, page_idx, self.rotation)]);

        for adjacent_page in [
            page_idx.checked_sub(1),
            (page_idx + 1 < self.pages.len()).then_some(page_idx + 1),
        ]
        .into_iter()
        .flatten()
        {
            let scale = self.presentation_scale_for_page(adjacent_page);
            let image_bounds =
                self.presentation_image_bounds_for_page(adjacent_page, bounds, scale);
            let image_bounds = Self::pixel_aligned_image_bounds(image_bounds, scale_factor);
            let key = self.render_key(adjacent_page, image_bounds, scale_factor);
            if key.pixel_count() <= MAX_PREFETCH_RENDER_PIXELS {
                self.request_render(key, cx);
            }
        }

        let image = self.cached_or_request_render(key, cx);
        self.assemble_paint_data(
            vec![PaintPage {
                bounds: image_bounds,
                image,
            }],
            window,
        )
    }

    fn paint_data(
        &mut self,
        bounds: Bounds<Pixels>,
        scale_factor: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaintData> {
        let viewport_width = bounds.size.width.as_f32().max(1.0);
        self.viewport_width = viewport_width;
        self.viewport_height = bounds.size.height.as_f32().max(1.0);
        self.schedule_reflow_for_viewport(viewport_width, cx);
        self.base_scale = if self.capabilities.reflow {
            1.0
        } else {
            viewport_width / self.max_page_width.max(1.0)
        };
        if self.is_presentation_mode() {
            return Ok(self.presentation_paint_data(bounds, scale_factor, window, cx));
        }
        self.clamp_scroll();

        self.page_image_bounds.clear();
        self.page_image_bounds
            .resize_with(self.pages.len(), || None);

        let scale = self.effective_scale();
        self.document_scroll.update(
            bounds,
            point(px(0.0), px(-self.scroll_y_doc * scale)),
            point(px(0.0), px(self.max_scroll_y_doc() * scale)),
        );
        let mut visible_pages = Vec::new();
        let viewport_top = bounds.origin.y.as_f32();
        let viewport_bottom = viewport_top + self.viewport_height;

        for page_idx in 0..self.pages.len() {
            let (image_bounds, _) =
                self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
            let page_top = image_bounds.origin.y.as_f32();
            let page_bottom = page_top + image_bounds.size.height.as_f32();

            if page_bottom >= viewport_top && page_top <= viewport_bottom {
                self.page_image_bounds[page_idx] = Some(image_bounds);
                visible_pages.push(page_idx);
            }
        }

        let mut visible_page_renders = Vec::with_capacity(visible_pages.len());
        for page_idx in visible_pages {
            let (image_bounds, key) =
                self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
            visible_page_renders.push((page_idx, image_bounds, key));
        }
        let doc_id = self.doc_id;
        cx.global_mut::<RenderCache>().set_protected_pages(
            visible_page_renders
                .iter()
                .map(|(page_idx, _, key)| (doc_id, *page_idx, key.rotation)),
        );

        if let (Some((first, _, _)), Some((last, _, _))) =
            (visible_page_renders.first(), visible_page_renders.last())
        {
            let start = first.saturating_sub(1);
            let end = (*last + 1).min(self.pages.len().saturating_sub(1));
            for page_idx in start..=end {
                if page_idx >= *first && page_idx <= *last {
                    continue;
                }

                let (_, key) =
                    self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
                if key.pixel_count() <= MAX_PREFETCH_RENDER_PIXELS {
                    self.request_render(key, cx);
                }
            }
        }

        let mut pages = Vec::with_capacity(visible_page_renders.len());
        let mut has_rendered_page = false;
        for (_, image_bounds, key) in visible_page_renders {
            let image = self.cached_or_request_render(key, cx);
            has_rendered_page |= image.is_some();
            pages.push(PaintPage {
                bounds: image_bounds,
                image,
            });
        }
        if has_rendered_page {
            self.start_text_indexing(cx);
        }

        Ok(self.assemble_paint_data(pages, window))
    }

    fn image_bounds_for_page(
        &self,
        page_idx: usize,
        bounds: Bounds<Pixels>,
        scale: f32,
    ) -> Bounds<Pixels> {
        let page = &self.pages[page_idx];
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let image_width = px(page_width * scale);
        let image_height = px(page_height * scale);
        let origin = point(
            bounds.origin.x + (bounds.size.width - image_width) / 2.0
                - px(self.scroll_x_doc * scale),
            bounds.origin.y + px((page.offset_y - self.scroll_y_doc) * scale),
        );

        Bounds::new(origin, size(image_width, image_height))
    }

    fn image_bounds_and_render_key(
        &self,
        page_idx: usize,
        bounds: Bounds<Pixels>,
        scale: f32,
        scale_factor: f32,
    ) -> (Bounds<Pixels>, RenderKey) {
        let image_bounds = self.image_bounds_for_page(page_idx, bounds, scale);
        let image_bounds = Self::pixel_aligned_image_bounds(image_bounds, scale_factor);
        let key = self.render_key(page_idx, image_bounds, scale_factor);
        (image_bounds, key)
    }

    fn render_key(
        &self,
        page_idx: usize,
        image_bounds: Bounds<Pixels>,
        scale_factor: f32,
    ) -> RenderKey {
        let scale_factor = scale_factor.max(0.001);
        let requested_width = (image_bounds.size.width.as_f32() * scale_factor)
            .ceil()
            .max(1.0);
        let requested_height = (image_bounds.size.height.as_f32() * scale_factor)
            .ceil()
            .max(1.0);
        let max_dimension = MAX_RENDER_TEXTURE_DIMENSION as f32;
        let texture_scale = (max_dimension / requested_width)
            .min(max_dimension / requested_height)
            .min(1.0);

        RenderKey {
            doc_id: self.doc_id,
            page_idx,
            rotation: self.rotation,
            physical_width: (requested_width * texture_scale)
                .round()
                .clamp(1.0, max_dimension) as u32,
            physical_height: (requested_height * texture_scale)
                .round()
                .clamp(1.0, max_dimension) as u32,
        }
    }

    fn pixel_aligned_image_bounds(
        image_bounds: Bounds<Pixels>,
        scale_factor: f32,
    ) -> Bounds<Pixels> {
        let scale_factor = scale_factor.max(0.001);
        let physical_width = (image_bounds.size.width.as_f32() * scale_factor)
            .ceil()
            .max(1.0);
        let physical_height = (image_bounds.size.height.as_f32() * scale_factor)
            .ceil()
            .max(1.0);

        Bounds::new(
            image_bounds.origin,
            size(
                px(physical_width / scale_factor),
                px(physical_height / scale_factor),
            ),
        )
    }

    fn effective_scale(&self) -> f32 {
        (self.base_scale * self.zoom).max(0.001)
    }

    fn rotated_dimensions(width: f32, height: f32, rotation: DocumentRotation) -> (f32, f32) {
        if rotation.swaps_width_and_height() {
            (height, width)
        } else {
            (width, height)
        }
    }

    fn page_dimensions_for(page: &PdfPage, rotation: DocumentRotation) -> (f32, f32) {
        Self::rotated_dimensions(page.width, page.height, rotation)
    }

    fn page_dimensions(&self, page_idx: usize) -> (f32, f32) {
        Self::page_dimensions_for(&self.pages[page_idx], self.rotation)
    }

    fn page_to_view_matrix(page_bounds: Rect, rotation: DocumentRotation) -> Matrix {
        Self::render_matrix(
            page_bounds,
            rotation,
            1.0,
            1.0,
            mupdf::Point { x: 0.0, y: 0.0 },
        )
    }

    fn pdf_point_to_view(
        page_bounds: Rect,
        rotation: DocumentRotation,
        point: mupdf::Point,
    ) -> mupdf::Point {
        point.transform(&Self::page_to_view_matrix(page_bounds, rotation))
    }

    fn view_point_to_pdf(
        page_bounds: Rect,
        rotation: DocumentRotation,
        point: mupdf::Point,
    ) -> mupdf::Point {
        let inverse = Self::page_to_view_matrix(page_bounds, rotation)
            .invert()
            .expect("document rotation matrix is invertible");
        point.transform(&inverse)
    }

    fn pdf_rect_to_view(page_bounds: Rect, rotation: DocumentRotation, rect: Rect) -> Rect {
        rect.transform(&Self::page_to_view_matrix(page_bounds, rotation))
    }

    fn view_rect_to_pdf(page_bounds: Rect, rotation: DocumentRotation, rect: Rect) -> Rect {
        let inverse = Self::page_to_view_matrix(page_bounds, rotation)
            .invert()
            .expect("document rotation matrix is invertible");
        rect.transform(&inverse)
    }

    fn render_matrix(
        page_bounds: Rect,
        rotation: DocumentRotation,
        scale_x: f32,
        scale_y: f32,
        view_origin: mupdf::Point,
    ) -> Matrix {
        let width = page_bounds.x1 - page_bounds.x0;
        let height = page_bounds.y1 - page_bounds.y0;
        let (a, b, c, d, tx, ty) = match rotation.quarter_turns() {
            0 => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            1 => (0.0, 1.0, -1.0, 0.0, height, 0.0),
            2 => (-1.0, 0.0, 0.0, -1.0, width, height),
            3 => (0.0, -1.0, 1.0, 0.0, 0.0, width),
            _ => unreachable!("document rotation is normalized"),
        };
        Matrix::new(
            a * scale_x,
            b * scale_y,
            c * scale_x,
            d * scale_y,
            (tx - a * page_bounds.x0 - c * page_bounds.y0 - view_origin.x) * scale_x,
            (ty - b * page_bounds.x0 - d * page_bounds.y0 - view_origin.y) * scale_y,
        )
    }

    fn max_scroll_y_doc(&self) -> f32 {
        let viewport_height_doc = self.viewport_height / self.effective_scale();
        (self.content_height_doc - viewport_height_doc).max(0.0)
    }

    fn max_scroll_x_doc(&self) -> f32 {
        let viewport_width_doc = self.viewport_width / self.effective_scale();
        ((self.max_page_width - viewport_width_doc) / 2.0).max(0.0)
    }

    fn recompute_layout(&mut self, page_gap: f32) {
        let mut offset_y = 0.0;
        let mut max_page_width = 0.0_f32;
        for page in &mut self.pages {
            let (width, height) = Self::page_dimensions_for(page, self.rotation);
            page.offset_y = offset_y;
            offset_y += height + page_gap;
            max_page_width = max_page_width.max(width);
        }

        self.max_page_width = max_page_width.max(1.0);
        self.content_height_doc = if self.pages.is_empty() {
            0.0
        } else {
            offset_y - page_gap
        };
        self.clamp_scroll();
    }

    fn schedule_reflow_for_viewport(&mut self, viewport_width: f32, cx: &mut Context<Self>) {
        if !self.capabilities.reflow {
            return;
        }
        let desired_width = self
            .epub_preferences
            .layout_for_viewport(viewport_width, 960.0)
            .width
            .round();
        let current_width = match self.document_layout {
            DocumentLayout::Reflow(layout) => layout.width.round(),
            DocumentLayout::Fixed => return,
        };
        if (desired_width - current_width).abs() < 1.0 {
            return;
        }

        self.reflow_resize_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            _ = this.update(cx, |this, cx| {
                this.reflow_resize_task = None;
                this.reflow_to_viewport(desired_width, cx);
            });
        }));
    }

    fn reflow_to_viewport(&mut self, viewport_width: f32, cx: &mut Context<Self>) {
        if !self.capabilities.reflow {
            return;
        }
        let layout = self
            .epub_preferences
            .layout_for_viewport(viewport_width, 960.0);
        if self.document_layout == DocumentLayout::Reflow(layout) {
            return;
        }
        let position = self
            .pending_semantic_position
            .clone()
            .or_else(|| self.pending_initial_position.clone())
            .filter(DocumentPosition::is_reflow)
            .unwrap_or_else(|| self.current_document_position());
        self.pending_initial_position = Some(position);
        self.pending_renders.clear();
        self.document_layout = DocumentLayout::Reflow(layout);
        self.request_reload(cx);
    }

    fn update_epub_preferences(
        &mut self,
        update: impl FnOnce(&mut EpubReaderPreferences),
        cx: &mut Context<Self>,
    ) {
        if !self.capabilities.reflow {
            return;
        }
        let mut preferences = self.epub_preferences;
        update(&mut preferences);
        preferences = preferences.sanitized();
        if preferences == self.epub_preferences {
            return;
        }
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_epub_reader_preferences(&self.path, preferences)
        {
            eprintln!("failed to save EPUB reader preferences: {error:#}");
            return;
        }
        self.epub_preferences = preferences;
        let position = self
            .pending_semantic_position
            .clone()
            .or_else(|| self.pending_initial_position.clone())
            .filter(DocumentPosition::is_reflow)
            .unwrap_or_else(|| self.current_document_position());
        self.pending_initial_position = Some(position);
        self.pending_renders.clear();
        self.document_layout = DocumentLayout::Reflow(
            preferences.layout_for_viewport(self.viewport_width.max(240.0), 960.0),
        );
        self.request_reload(cx);
        cx.notify();
    }

    fn increase_epub_font_size(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.font_size += 1.0, cx);
    }

    fn decrease_epub_font_size(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.font_size -= 1.0, cx);
    }

    fn increase_epub_line_height(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.line_height += 0.1, cx);
    }

    fn decrease_epub_line_height(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.line_height -= 0.1, cx);
    }

    fn increase_epub_horizontal_margin(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.horizontal_margin += 4.0, cx);
    }

    fn decrease_epub_horizontal_margin(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.horizontal_margin -= 4.0, cx);
    }

    fn increase_epub_max_content_width(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.max_content_width += 40.0, cx);
    }

    fn decrease_epub_max_content_width(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.max_content_width -= 40.0, cx);
    }

    fn toggle_epub_publisher_styles(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(
            |preferences| preferences.use_publisher_styles = !preferences.use_publisher_styles,
            cx,
        );
    }

    fn set_epub_theme(&mut self, theme: EpubTheme, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.theme = theme, cx);
    }

    fn reset_epub_preferences(&mut self, cx: &mut Context<Self>) {
        if !self.capabilities.reflow {
            return;
        }
        let preferences = cx.global::<SettingsStore>().settings.reading.sanitized();
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .clear_epub_reader_preferences(&self.path)
        {
            eprintln!("failed to clear EPUB reader preferences: {error:#}");
            return;
        }
        if preferences == self.epub_preferences {
            cx.notify();
            return;
        }
        let position = self
            .pending_semantic_position
            .clone()
            .or_else(|| self.pending_initial_position.clone())
            .filter(DocumentPosition::is_reflow)
            .unwrap_or_else(|| self.current_document_position());
        self.pending_initial_position = Some(position);
        self.epub_preferences = preferences;
        self.document_layout = DocumentLayout::Reflow(
            preferences.layout_for_viewport(self.viewport_width.max(240.0), 960.0),
        );
        self.request_reload(cx);
        cx.notify();
    }

    fn toggle_epub_style_panel(&mut self, position: GpuiPoint<Pixels>, cx: &mut Context<Self>) {
        if self.epub_style_panel_position.take().is_none() {
            self.close_context_menu(cx);
            self.close_grid_overview_actions(cx);
            self.close_thumbnail_sidebar_actions(cx);
            self.epub_style_panel_position = Some(point(
                position.x,
                position.y - px(FOOTBAR_BUTTON_HEIGHT / 2.0),
            ));
        }
        cx.notify();
    }

    fn close_epub_style_panel(&mut self, cx: &mut Context<Self>) {
        if self.epub_style_panel_position.take().is_some() {
            cx.notify();
        }
    }

    fn clamp_scroll(&mut self) {
        let max_scroll_x = self.max_scroll_x_doc();
        self.scroll_x_doc = self.scroll_x_doc.clamp(-max_scroll_x, max_scroll_x);
        self.scroll_y_doc = self.scroll_y_doc.clamp(0.0, self.max_scroll_y_doc());
        self.target_scroll_x_doc = self.target_scroll_x_doc.clamp(-max_scroll_x, max_scroll_x);
        self.target_scroll_y_doc = self.target_scroll_y_doc.clamp(0.0, self.max_scroll_y_doc());
    }

    fn set_scroll_doc(
        &mut self,
        scroll_x_doc: f32,
        scroll_y_doc: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let previous_x = self.scroll_x_doc;
        let previous_y = self.scroll_y_doc;
        self.scroll_x_doc = scroll_x_doc;
        self.scroll_y_doc = scroll_y_doc;
        self.target_scroll_x_doc = scroll_x_doc;
        self.target_scroll_y_doc = scroll_y_doc;
        self.clamp_scroll();

        let changed = (self.scroll_x_doc - previous_x).abs() > f32::EPSILON
            || (self.scroll_y_doc - previous_y).abs() > f32::EPSILON;
        if changed {
            self.hovered_link = None;
            self.pending_link = None;
            self.schedule_position_save(cx);
            self.emit_linked_viewport(cx);
            cx.notify();
        }

        changed
    }

    fn set_scroll_doc_target(
        &mut self,
        scroll_x_doc: f32,
        scroll_y_doc: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let previous_x = self.target_scroll_x_doc;
        let previous_y = self.target_scroll_y_doc;
        self.target_scroll_x_doc = scroll_x_doc;
        self.target_scroll_y_doc = scroll_y_doc;
        self.clamp_scroll();

        let smooth_scrolling = cx
            .global::<SettingsStore>()
            .settings
            .behavior
            .smooth_scrolling;
        if smooth_scrolling {
            let viewport_height_doc = self.viewport_height / self.effective_scale();
            let max_animated_distance = viewport_height_doc * SMOOTH_SCROLL_LARGE_JUMP_VIEWPORTS;
            let delta_y = self.target_scroll_y_doc - self.scroll_y_doc;
            if delta_y.abs() > max_animated_distance {
                self.scroll_y_doc =
                    self.target_scroll_y_doc - delta_y.signum() * max_animated_distance;
            }
        } else {
            self.scroll_x_doc = self.target_scroll_x_doc;
            self.scroll_y_doc = self.target_scroll_y_doc;
        }

        let changed = (self.target_scroll_x_doc - previous_x).abs() > f32::EPSILON
            || (self.target_scroll_y_doc - previous_y).abs() > f32::EPSILON;
        if changed {
            self.hovered_link = None;
            self.pending_link = None;
            self.schedule_position_save(cx);
            self.emit_linked_viewport(cx);
            cx.notify();
        }

        changed
    }

    fn step_scroll_animation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let smooth_scrolling = cx
            .global::<SettingsStore>()
            .settings
            .behavior
            .smooth_scrolling;
        let changed_x = advance_toward(
            &mut self.scroll_x_doc,
            self.target_scroll_x_doc,
            smooth_scrolling,
        );
        let changed_y = advance_toward(
            &mut self.scroll_y_doc,
            self.target_scroll_y_doc,
            smooth_scrolling,
        );
        if changed_x || changed_y {
            window.request_animation_frame();
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_document_translation_jobs(
        &mut self,
        jobs: Entity<DocumentTranslationJobManager>,
        cx: &mut Context<Self>,
    ) {
        self.document_translation_jobs = jobs;
        cx.notify();
    }

    pub fn format(&self) -> DocumentFormat {
        self.format
    }

    pub fn capabilities(&self) -> DocumentCapabilities {
        self.capabilities
    }

    pub fn file_name(&self) -> SharedString {
        self.file_name.clone()
    }

    fn restore_position(&mut self, position: DocumentPosition) {
        if self.capabilities.reflow
            && let Some(reflow) = position.semantic.as_ref()
        {
            self.zoom = position.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
            let progress_scroll = reflow.progress.clamp(0.0, 1.0) * self.max_scroll_y_doc();
            if self.glyphs.is_empty() {
                self.scroll_x_doc = 0.0;
                self.scroll_y_doc = progress_scroll;
                self.target_scroll_x_doc = 0.0;
                self.target_scroll_y_doc = progress_scroll;
                self.clamp_scroll();
                self.pending_semantic_position = Some(position);
                return;
            }
            if !reflow.anchor.is_empty()
                && let Some(glyph_ix) = Self::resolve_semantic_anchor(&self.glyphs, &reflow.anchor)
                && let Some(scroll_y) = self.scroll_y_for_glyph(glyph_ix)
            {
                self.scroll_x_doc = 0.0;
                self.scroll_y_doc = scroll_y
                    - reflow.viewport_fraction * (self.viewport_height / self.effective_scale());
                self.target_scroll_x_doc = 0.0;
                self.target_scroll_y_doc = self.scroll_y_doc;
                self.clamp_scroll();
                self.pending_semantic_position = None;
                return;
            }
            self.scroll_x_doc = 0.0;
            self.scroll_y_doc = progress_scroll;
            self.target_scroll_x_doc = 0.0;
            self.target_scroll_y_doc = progress_scroll;
            self.clamp_scroll();
            self.pending_semantic_position = None;
            return;
        }
        self.zoom = position.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.scroll_x_doc = position.scroll_x_doc;
        self.scroll_y_doc = position.scroll_y_doc;
        self.target_scroll_x_doc = position.scroll_x_doc;
        self.target_scroll_y_doc = position.scroll_y_doc;
        self.clamp_scroll();
    }

    pub fn current_document_position(&self) -> DocumentPosition {
        if self.capabilities.reflow {
            let page_idx = self.page_index_for_scroll_y(self.target_scroll_y_doc);
            let progress = if self.max_scroll_y_doc() > 0.0 {
                self.target_scroll_y_doc / self.max_scroll_y_doc()
            } else {
                0.0
            };
            if self.glyphs.is_empty() {
                return DocumentPosition::reflow(
                    SemanticTextAnchor::empty(),
                    0.0,
                    progress,
                    self.zoom,
                    page_idx + 1,
                );
            }
            let y_in_page = self
                .pages
                .get(page_idx)
                .map(|page| (self.target_scroll_y_doc - page.offset_y).max(0.0))
                .unwrap_or(0.0);
            let glyph_ix = self
                .pages
                .get(page_idx)
                .and_then(|page| {
                    page.glyph_range
                        .clone()
                        .find(|glyph_ix| {
                            self.glyphs
                                .get(*glyph_ix)
                                .is_some_and(|glyph| Self::glyph_line_rect(glyph).y1 >= y_in_page)
                        })
                        .or(Some(page.glyph_range.start))
                })
                .unwrap_or(0)
                .min(self.glyphs.len().saturating_sub(1));
            let anchor = Self::semantic_anchor_for_glyph(&self.glyphs, glyph_ix);
            let viewport_height_doc = self.viewport_height / self.effective_scale();
            let viewport_fraction = if viewport_height_doc > 0.0 {
                self.scroll_y_for_glyph(glyph_ix)
                    .map(|anchor_y| {
                        ((anchor_y - self.target_scroll_y_doc) / viewport_height_doc)
                            .clamp(0.0, 1.0)
                    })
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            return DocumentPosition::reflow(
                anchor,
                viewport_fraction,
                progress,
                self.zoom,
                page_idx + 1,
            );
        }
        if let Some(page_idx) = self.presentation_page_index() {
            return DocumentPosition::fixed(
                0.0,
                self.page_scroll_y_doc(page_idx),
                self.zoom,
                page_idx + 1,
            );
        }

        DocumentPosition::fixed(
            self.target_scroll_x_doc,
            self.target_scroll_y_doc,
            self.zoom,
            self.page_index_for_scroll_y(self.target_scroll_y_doc) + 1,
        )
    }

    fn semantic_anchor_for_glyph(glyphs: &[Glyph], glyph_ix: usize) -> SemanticTextAnchor {
        const QUOTE_LEN: usize = 32;
        const CONTEXT_LEN: usize = 24;

        let glyph_ix = glyph_ix.min(glyphs.len().saturating_sub(1));
        let quote_end = (glyph_ix + QUOTE_LEN).min(glyphs.len());
        let prefix_start = glyph_ix.saturating_sub(CONTEXT_LEN);
        let suffix_end = (quote_end + CONTEXT_LEN).min(glyphs.len());
        SemanticTextAnchor::new(
            glyph_ix,
            glyphs[glyph_ix..quote_end]
                .iter()
                .map(|glyph| glyph.c)
                .collect::<String>(),
            glyphs[prefix_start..glyph_ix]
                .iter()
                .map(|glyph| glyph.c)
                .collect::<String>(),
            glyphs[quote_end..suffix_end]
                .iter()
                .map(|glyph| glyph.c)
                .collect::<String>(),
        )
    }

    fn resolve_semantic_anchor(glyphs: &[Glyph], anchor: &SemanticTextAnchor) -> Option<usize> {
        if glyphs.is_empty() {
            return None;
        }
        let quote = anchor.quote.chars().collect::<Vec<_>>();
        let candidate = usize::try_from(anchor.logical_offset).ok()?;
        if candidate < glyphs.len() && Self::glyph_chars_match(glyphs, candidate, &quote) {
            return Some(candidate);
        }
        if quote.is_empty() {
            return Some(candidate.min(glyphs.len() - 1));
        }

        let mut fallback = None;
        for start in 0..glyphs.len() {
            if !Self::glyph_chars_match(glyphs, start, &quote) {
                continue;
            }
            fallback.get_or_insert(start);
            let prefix_start = start.saturating_sub(anchor.prefix.chars().count());
            let prefix = glyphs[prefix_start..start]
                .iter()
                .map(|glyph| glyph.c)
                .collect::<String>();
            let suffix_start = (start + quote.len()).min(glyphs.len());
            let suffix_end = (suffix_start + anchor.suffix.chars().count()).min(glyphs.len());
            let suffix = glyphs[suffix_start..suffix_end]
                .iter()
                .map(|glyph| glyph.c)
                .collect::<String>();
            if prefix.ends_with(&anchor.prefix) && suffix.starts_with(&anchor.suffix) {
                return Some(start);
            }
        }
        fallback.or_else(|| Some(candidate.min(glyphs.len() - 1)))
    }

    fn glyph_chars_match(glyphs: &[Glyph], start: usize, expected: &[char]) -> bool {
        start + expected.len() <= glyphs.len()
            && glyphs[start..start + expected.len()]
                .iter()
                .map(|glyph| glyph.c)
                .eq(expected.iter().copied())
    }

    fn scroll_y_for_glyph(&self, glyph_ix: usize) -> Option<f32> {
        let glyph = self.glyphs.get(glyph_ix)?;
        let page = self.pages.get(glyph.page_idx)?;
        Some(page.offset_y + Self::glyph_line_rect(glyph).y0)
    }

    fn semantic_range_for_selection(&self, start: usize, end: usize) -> SemanticTextRange {
        SemanticTextRange {
            start: Self::semantic_anchor_for_glyph(&self.glyphs, start),
            end: Self::semantic_anchor_for_glyph(&self.glyphs, end),
        }
    }

    fn refresh_semantic_annotations(&mut self) {
        let highlight_ranges = self
            .highlights
            .iter()
            .enumerate()
            .filter_map(|(ix, highlight)| {
                let range = highlight.semantic_range.as_ref()?;
                let start = Self::resolve_semantic_anchor(&self.glyphs, &range.start)?;
                let end = Self::resolve_semantic_anchor(&self.glyphs, &range.end)?;
                Some((ix, start.min(end)..start.max(end).saturating_add(1)))
            })
            .collect::<Vec<_>>();
        for (ix, range) in highlight_ranges {
            self.highlights[ix].rects = self.highlight_rects_for_range(range);
        }

        let note_ranges = self
            .notes
            .iter()
            .enumerate()
            .filter_map(|(ix, note)| {
                let range = note.semantic_range.as_ref()?;
                let start = Self::resolve_semantic_anchor(&self.glyphs, &range.start)?;
                let end = Self::resolve_semantic_anchor(&self.glyphs, &range.end)?;
                Some((ix, start.min(end)..start.max(end).saturating_add(1)))
            })
            .collect::<Vec<_>>();
        for (ix, range) in note_ranges {
            self.notes[ix].rects = self.highlight_rects_for_range(range);
        }
    }

    fn sync_document_edits_from_store(&mut self, cx: &mut Context<Self>) {
        let (document_edit_revision, bookmarks_revision) = {
            let store = cx.global::<LibraryStore>();
            (
                store.document_edit_revision(&self.path),
                store.bookmarks_revision(),
            )
        };
        let annotations_changed =
            document_edit_revision.annotations != self.document_edit_revision.annotations;
        let rotation_changed =
            document_edit_revision.rotation != self.document_edit_revision.rotation;
        let bookmarks_changed = bookmarks_revision != self.bookmarks_revision;
        if !annotations_changed && !rotation_changed && !bookmarks_changed {
            return;
        }

        self.document_edit_revision = document_edit_revision;
        self.bookmarks_revision = bookmarks_revision;
        let mut visible_state_changed = false;

        if annotations_changed {
            visible_state_changed = true;
            let (highlights, notes) = {
                let store = cx.global::<LibraryStore>();
                (
                    store.highlights_for(&self.path),
                    store.notes_for(&self.path),
                )
            };
            self.highlights = highlights;
            self.notes = notes;
            if self.capabilities.reflow && !self.glyphs.is_empty() {
                self.refresh_semantic_annotations();
            }

            let highlight_ids = self
                .highlights
                .iter()
                .map(|highlight| highlight.id)
                .collect::<HashSet<_>>();
            let note_ids = self
                .notes
                .iter()
                .map(|note| note.id)
                .collect::<HashSet<_>>();
            retain_valid_annotation_id(&mut self.selected_highlight, &highlight_ids);
            let stale_context_target = self
                .context_menu_highlight
                .is_some_and(|id| !highlight_ids.contains(&id))
                || self
                    .context_menu_note
                    .is_some_and(|id| !note_ids.contains(&id));
            if stale_context_target {
                self.close_context_menu(cx);
            }
            if retain_valid_annotation_id(&mut self.hovered_note, &note_ids) {
                self.hovered_note_position = None;
            }
            retain_valid_annotation_id(&mut self.notes_sidebar_selected, &note_ids);
            if matches!(
                self.note_target,
                Some(NoteTarget::Edit { id }) if !note_ids.contains(&id)
            ) {
                self.note_target = None;
            }
            self.refresh_notes_sidebar_entries();
            if let Some(palette) = self.notes_palette.clone() {
                let entries = self.note_palette_entries();
                palette.update(cx, |picker, cx| {
                    picker.update_delegate(move |delegate| delegate.replace_entries(entries), cx);
                });
            }
        }

        if rotation_changed {
            visible_state_changed = true;
            let rotation = cx.global::<LibraryStore>().document_rotation(&self.path);
            self.apply_stored_rotation(rotation, cx);
        }

        if bookmarks_changed && let Some(palette) = self.bookmark_palette.clone() {
            visible_state_changed = true;
            let (bookmarks, current_path) = {
                let store = cx.global::<LibraryStore>();
                (store.bookmarks().to_vec(), self.path.clone())
            };
            palette.update(cx, |picker, cx| {
                picker.update_delegate(
                    move |delegate| delegate.replace_bookmarks(bookmarks, current_path),
                    cx,
                );
            });
        }

        if visible_state_changed {
            cx.notify();
        }
    }

    fn undo_document_edit(&mut self, cx: &mut Context<Self>) {
        match cx.update_global::<LibraryStore, _>(|store, _| store.undo_document(&self.path)) {
            Ok(_) => {}
            Err(error) => eprintln!("failed to undo document edit: {error:#}"),
        }
    }

    fn redo_document_edit(&mut self, cx: &mut Context<Self>) {
        match cx.update_global::<LibraryStore, _>(|store, _| store.redo_document(&self.path)) {
            Ok(_) => {}
            Err(error) => eprintln!("failed to redo document edit: {error:#}"),
        }
    }

    pub fn set_translated_view(&mut self, translated: bool, cx: &mut Context<Self>) {
        if translated && !self.capabilities.translated_document {
            return;
        }
        self.view_language = if translated {
            PdfViewLanguage::Translated
        } else {
            PdfViewLanguage::Original
        };
        if translated {
            self.request_document_translation(cx);
        }
        cx.notify();
    }

    fn viewport_center_pdf_anchor(&self) -> Option<(usize, mupdf::Point)> {
        if self.pages.is_empty() {
            return None;
        }
        let scale = self.effective_scale();
        let center_y = self.target_scroll_y_doc + self.viewport_height / (2.0 * scale);
        let page_idx = self.page_index_for_doc_y(center_y);
        let page = self.pages.get(page_idx)?;
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let point = mupdf::Point {
            x: (page_width / 2.0 + self.target_scroll_x_doc).clamp(0.0, page_width),
            y: (center_y - page.offset_y).clamp(0.0, page_height),
        };
        Some((
            page_idx,
            Self::view_point_to_pdf(page.bounds, self.rotation, point),
        ))
    }

    fn set_rotation_state(&mut self, rotation: DocumentRotation, cx: &mut Context<Self>) {
        if self.rotation == rotation || self.capabilities.reflow {
            return;
        }
        self.rotation = rotation;
        self.pending_renders.clear();
        self.page_image_bounds.fill(None);
        self.diff_flashes.clear();
        self.diff_pending_pages.clear();
        self.translated_text_layouts.clear();
        self.region_drag = None;
        self.context_menu_position = None;
        self.jump_back.clear();
        self.jump_forward.clear();
        let page_gap = cx.global::<SettingsStore>().resolved_theme.page_gap;
        self.recompute_layout(page_gap);
        self.base_scale = self.viewport_width.max(1.0) / self.max_page_width.max(1.0);
        cx.notify();
    }

    fn apply_stored_rotation(
        &mut self,
        rotation: DocumentRotation,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.rotation == rotation || self.capabilities.reflow {
            return false;
        }
        let anchor = self.viewport_center_pdf_anchor();
        self.set_rotation_state(rotation, cx);
        if !self.is_presentation_mode()
            && let Some((page_idx, source_point)) = anchor
            && let Some(page) = self.pages.get(page_idx)
        {
            let point = Self::pdf_point_to_view(page.bounds, self.rotation, source_point);
            let (page_width, _) = self.page_dimensions(page_idx);
            let scale = self.effective_scale();
            self.scroll_x_doc = point.x - page_width / 2.0;
            self.scroll_y_doc = page.offset_y + point.y - self.viewport_height / (2.0 * scale);
            self.target_scroll_x_doc = self.scroll_x_doc;
            self.target_scroll_y_doc = self.scroll_y_doc;
            self.clamp_scroll();
        }
        self.schedule_position_save(cx);
        true
    }

    fn rotate_to(&mut self, rotation: DocumentRotation, cx: &mut Context<Self>) {
        if self.rotation == rotation || self.capabilities.reflow {
            return;
        }
        match cx.update_global::<LibraryStore, _>(|store, _| {
            store.set_document_rotation(&self.path, rotation)
        }) {
            Ok(true) => {
                self.apply_stored_rotation(rotation, cx);
            }
            Ok(false) => {}
            Err(error) => {
                eprintln!("failed to save document rotation: {error:#}");
            }
        }
    }

    fn rotate_clockwise(&mut self, cx: &mut Context<Self>) {
        self.rotate_to(self.rotation.clockwise(), cx);
    }

    fn rotate_counterclockwise(&mut self, cx: &mut Context<Self>) {
        self.rotate_to(self.rotation.counterclockwise(), cx);
    }

    pub fn apply_linked_viewport(&mut self, viewport: LinkedViewport, cx: &mut Context<Self>) {
        if self.pages.get(viewport.page_idx).is_none() {
            return;
        }
        self.suppress_linked_events = true;
        self.set_rotation_state(viewport.rotation, cx);
        self.zoom = viewport.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let page = &self.pages[viewport.page_idx];
        let (_, page_height) = self.page_dimensions(viewport.page_idx);
        self.set_scroll_doc(
            self.scroll_x_doc,
            page.offset_y + viewport.y_in_page.max(0.0).min(page_height),
            cx,
        );
        self.suppress_linked_events = false;
    }

    fn linked_block_anchor(&self, block_id: u32) -> Option<LinkedBlockAnchor> {
        let block = self.text_blocks.iter().find(|block| block.id == block_id)?;
        Some(LinkedBlockAnchor {
            block_id,
            page_idx: block.page_idx,
            x0: block.bounds.x0,
            y0: block.bounds.y0,
            x1: block.bounds.x1,
            y1: block.bounds.y1,
        })
    }

    fn map_linked_block(&self, anchor: LinkedBlockAnchor) -> Option<u32> {
        let anchor_center = ((anchor.x0 + anchor.x1) * 0.5, (anchor.y0 + anchor.y1) * 0.5);
        self.text_blocks
            .iter()
            .filter(|block| block.page_idx == anchor.page_idx)
            .min_by(|left, right| {
                let distance = |block: &TextBlock| {
                    let center = (
                        (block.bounds.x0 + block.bounds.x1) * 0.5,
                        (block.bounds.y0 + block.bounds.y1) * 0.5,
                    );
                    (center.0 - anchor_center.0).powi(2) + (center.1 - anchor_center.1).powi(2)
                };
                distance(left).total_cmp(&distance(right))
            })
            .map(|block| block.id)
    }

    pub fn apply_linked_hover(
        &mut self,
        anchor: Option<LinkedBlockAnchor>,
        cx: &mut Context<Self>,
    ) {
        let block_id = anchor.and_then(|anchor| self.map_linked_block(anchor));
        if self.external_hovered_block != block_id {
            self.external_hovered_block = block_id;
            cx.notify();
        }
    }

    pub fn apply_linked_selection(
        &mut self,
        selection: Option<LinkedBlockSelection>,
        cx: &mut Context<Self>,
    ) {
        let block_id = selection.and_then(|selection| self.map_linked_block(selection.anchor));
        if self.external_selected_block != block_id {
            self.external_selected_block = block_id;
            cx.notify();
        }
    }

    fn emit_linked_viewport(&mut self, cx: &mut Context<Self>) {
        if self.suppress_linked_events || self.pages.is_empty() {
            return;
        }
        self.linked_viewport_generation = self.linked_viewport_generation.wrapping_add(1);
        let page_idx = self.page_index_for_scroll_y(self.target_scroll_y_doc);
        let page = &self.pages[page_idx];
        let (_, page_height) = self.page_dimensions(page_idx);
        cx.emit(PdfViewEvent::ViewportChanged(LinkedViewport {
            page_idx,
            y_in_page: (self.target_scroll_y_doc - page.offset_y).clamp(0.0, page_height),
            zoom: self.zoom,
            rotation: self.rotation,
            generation: self.linked_viewport_generation,
        }));
    }

    fn set_hovered_text_block(&mut self, block_id: Option<u32>, cx: &mut Context<Self>) {
        if self.hovered_text_block != block_id {
            self.hovered_text_block = block_id;
            if !self.suppress_linked_events {
                cx.emit(PdfViewEvent::BlockHovered(
                    block_id.and_then(|block_id| self.linked_block_anchor(block_id)),
                ));
            }
            cx.notify();
        }
    }

    fn emit_linked_selection(&mut self, cx: &mut Context<Self>) {
        if self.suppress_linked_events {
            return;
        }
        self.linked_selection_generation = self.linked_selection_generation.wrapping_add(1);
        let selection = self.selection_bounds().and_then(|(start, _)| {
            self.glyphs.get(start).and_then(|glyph| {
                self.linked_block_anchor(glyph.block_idx)
                    .map(|anchor| LinkedBlockSelection {
                        anchor,
                        generation: self.linked_selection_generation,
                    })
            })
        });
        cx.emit(PdfViewEvent::BlockSelected(selection));
    }

    fn record_current_history_position(&self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let position = self.current_document_position();
        cx.global_mut::<LibraryStore>()
            .record_history(&path, position);
    }

    pub fn flush_current_history_position(&mut self, cx: &mut Context<Self>) {
        self.stage_current_history_position(cx);
        Self::persist_history_entry(&self.path, cx);
    }

    pub fn stage_current_history_position(&mut self, cx: &mut Context<Self>) {
        self.position_save_task.take();
        if self.loading_error.is_some() {
            return;
        }

        let position = if self.loading {
            self.pending_semantic_position
                .clone()
                .or_else(|| self.pending_initial_position.clone())
                .unwrap_or_else(|| self.current_document_position())
        } else {
            self.current_document_position()
        };
        cx.global_mut::<LibraryStore>()
            .record_history(&self.path, position);
    }

    fn persist_history_entry(path: &Path, cx: &mut Context<Self>) {
        if let Some(snapshot) = cx
            .global::<LibraryStore>()
            .history_entry_persistence_snapshot(path)
        {
            cx.background_spawn(async move {
                if let Err(error) = snapshot.persist() {
                    eprintln!("failed to save document history: {error:#}");
                }
            })
            .detach();
        }
    }

    fn schedule_position_save(&mut self, cx: &mut Context<Self>) {
        self.pending_semantic_position = None;
        self.pending_initial_position = None;
        self.record_current_history_position(cx);
        self.position_save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1000))
                .await;
            _ = this.update(cx, |this, cx| {
                Self::persist_history_entry(&this.path, cx);
            });
        }));
    }

    fn current_sidebar_widths(&self) -> SidebarWidths {
        SidebarWidths {
            thumbnails: self.thumbnail_sidebar_width,
            toc: self.toc_sidebar_width,
            notes: self.notes_sidebar_width,
            ai: self.ai_panel_width,
        }
    }

    fn schedule_sidebar_width_save(&mut self, cx: &mut Context<Self>) {
        self.sidebar_width_save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            _ = this.update(cx, |this, cx| {
                let widths = Self::sanitize_sidebar_widths(this.current_sidebar_widths());
                if let Err(error) = cx.global_mut::<LibraryStore>().set_sidebar_widths(widths) {
                    eprintln!("failed to save sidebar widths: {error:#}");
                }
            });
        }));
    }

    fn current_jump_position(&self) -> JumpPosition {
        if let Some(page_idx) = self.presentation_page_index() {
            return JumpPosition {
                scroll_x_doc: 0.0,
                scroll_y_doc: self.page_scroll_y_doc(page_idx),
            };
        }

        JumpPosition {
            scroll_x_doc: self.scroll_x_doc,
            scroll_y_doc: self.scroll_y_doc,
        }
    }

    fn push_jump_position(stack: &mut Vec<JumpPosition>, position: JumpPosition) {
        if stack.last().is_some_and(|last| {
            (last.scroll_x_doc - position.scroll_x_doc).abs() <= JUMP_POSITION_EPSILON_DOC
                && (last.scroll_y_doc - position.scroll_y_doc).abs() <= JUMP_POSITION_EPSILON_DOC
        }) {
            return;
        }

        stack.push(position);
        if stack.len() > MAX_JUMP_HISTORY {
            stack.remove(0);
        }
    }

    fn record_search_jump_origin(&mut self) {
        if self.position_from_search {
            return;
        }

        let position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, position);
        self.jump_forward.clear();
        self.position_from_search = true;
    }

    fn mark_manual_navigation(&mut self) {
        self.position_from_search = false;
        self.search.cancel_reveal_after_indexing();
    }

    fn jump_backward(&mut self, cx: &mut Context<Self>) {
        let Some(position) = self.jump_back.pop() else {
            return;
        };

        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_forward, current_position);
        if self.is_presentation_mode() {
            self.set_presentation_page(self.page_index_for_doc_y(position.scroll_y_doc), cx);
        } else {
            self.set_scroll_doc(position.scroll_x_doc, position.scroll_y_doc, cx);
        }
        self.mark_manual_navigation();
    }

    fn jump_forward(&mut self, cx: &mut Context<Self>) {
        let Some(position) = self.jump_forward.pop() else {
            return;
        };

        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        if self.is_presentation_mode() {
            self.set_presentation_page(self.page_index_for_doc_y(position.scroll_y_doc), cx);
        } else {
            self.set_scroll_doc(position.scroll_x_doc, position.scroll_y_doc, cx);
        }
        self.mark_manual_navigation();
    }

    fn request_render(&mut self, key: RenderKey, cx: &mut Context<Self>) {
        if !cx.global::<RenderCache>().contains_render(key) {
            self.start_background_render(key, cx);
        }
    }

    fn cached_or_request_render(
        &mut self,
        key: RenderKey,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        if let Some(image) = cx.global_mut::<RenderCache>().render(key) {
            return Some(image);
        }

        if !self.pending_renders.iter().any(|pending| {
            pending.key.page_idx == key.page_idx && pending.key.rotation == key.rotation
        }) {
            self.start_background_render(key, cx);
        }
        let (page_width, _) = self.page_dimensions(key.page_idx);
        let render_scale = key.physical_width as f32 / page_width;
        let render_cache = cx.global_mut::<RenderCache>();
        if let Some(image) =
            render_cache.scaled_render(self.doc_id, key.page_idx, key.rotation, render_scale)
        {
            return Some(image);
        }

        self.previous_doc_id.and_then(|previous_doc_id| {
            render_cache.scaled_render(previous_doc_id, key.page_idx, key.rotation, render_scale)
        })
    }

    fn insert_cached_render(
        &mut self,
        key: RenderKey,
        image: Arc<RenderImage>,
        cx: &mut Context<Self>,
    ) {
        let (page_width, _) = self.page_dimensions(key.page_idx);
        let render_scale = key.physical_width as f32 / page_width;
        let removed_images = cx
            .global_mut::<RenderCache>()
            .insert_render(key, render_scale, image);
        for image in removed_images {
            cx.drop_image(image, None);
        }
    }

    fn start_background_render(&mut self, key: RenderKey, cx: &mut Context<Self>) {
        if self.reload_task.is_some() {
            return;
        }
        if self.pending_renders.iter().any(|pending| {
            pending.key.page_idx == key.page_idx && pending.key.rotation == key.rotation
        }) {
            return;
        }
        self.pending_renders
            .retain(|pending| pending.key.page_idx != key.page_idx);
        if self.pending_renders.len() >= MAX_PENDING_PAGE_RENDERS {
            return;
        }

        let page = &self.pages[key.page_idx];
        let page_bounds = page.bounds;
        let source_page_width = page.width;
        let source_page_height = page.height;
        let (page_width, page_height) = Self::page_dimensions_for(page, key.rotation);
        let cached_display_list = cx
            .global_mut::<RenderCache>()
            .display_list(self.doc_id, key.page_idx);
        let snapshot = self.document_snapshot.clone();
        let document_layout = self.document_layout;

        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let display_list = match cached_display_list {
                        Some(display_list) => display_list,
                        None => Self::create_display_list(
                            &snapshot,
                            document_layout,
                            key.page_idx,
                            page_bounds,
                        )?,
                    };
                    let image = Self::render_page(
                        &display_list,
                        page_bounds,
                        source_page_width,
                        source_page_height,
                        key.rotation,
                        key.physical_width,
                        key.physical_height,
                    )?;
                    Ok::<_, anyhow::Error>((display_list, image))
                })
                .await;

            _ = this
                .update(cx, |this, cx| {
                    let Some(index) = this
                        .pending_renders
                        .iter()
                        .position(|pending| pending.key == key)
                    else {
                        return;
                    };
                    this.pending_renders.remove(index);

                    match result {
                        Ok((display_list, image)) => {
                            cx.global_mut::<RenderCache>().insert_display_list(
                                key.doc_id,
                                key.page_idx,
                                display_list,
                            );
                            let previous_image = this.take_previous_render_for_diff(key, cx);
                            this.insert_cached_render(key, image.clone(), cx);
                            if let Some(previous_image) = previous_image {
                                this.start_diff_flash_task(
                                    DiffRenderInput {
                                        key,
                                        page_bounds,
                                        page_width,
                                        page_height,
                                        rotation: key.rotation,
                                    },
                                    previous_image,
                                    image,
                                    cx,
                                );
                            }
                            cx.notify();
                        }
                        Err(error) => eprintln!("failed to render PDF page: {error:#}"),
                    }
                })
                .ok();
        });

        self.pending_renders
            .push(PendingRender { key, _task: task });
    }

    fn get_or_create_display_list(
        &self,
        page_idx: usize,
        cx: &mut Context<Self>,
    ) -> Result<Arc<DisplayList>> {
        if let Some(display_list) = cx
            .global_mut::<RenderCache>()
            .display_list(self.doc_id, page_idx)
        {
            return Ok(display_list);
        }

        let page_bounds = self
            .pages
            .get(page_idx)
            .ok_or_else(|| anyhow::anyhow!("PDF page index {page_idx} is out of bounds"))?
            .bounds;
        let display_list = Self::create_display_list(
            &self.document_snapshot,
            self.document_layout,
            page_idx,
            page_bounds,
        )?;
        Ok(cx
            .global_mut::<RenderCache>()
            .insert_display_list(self.doc_id, page_idx, display_list))
    }

    fn create_display_list(
        snapshot: &DocumentSnapshot,
        document_layout: DocumentLayout,
        page_idx: usize,
        page_bounds: Rect,
    ) -> Result<Arc<DisplayList>> {
        let document = snapshot.open_with_layout(document_layout)?.into_inner();
        let page = document.load_page(page_idx as i32)?;
        let display_list = Arc::new(DisplayList::new(page_bounds)?);
        let device = Device::from_display_list(&display_list)?;
        page.run(&device, &Matrix::IDENTITY)?;
        Ok(display_list)
    }

    fn take_previous_render_for_diff(
        &mut self,
        key: RenderKey,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        let previous_doc_id = self.previous_doc_id?;
        if !self.diff_pending_pages.remove(&key.page_idx) {
            return None;
        }

        let previous_key = RenderKey {
            doc_id: previous_doc_id,
            ..key
        };
        let (page_width, _) = self.page_dimensions(key.page_idx);
        let render_scale = key.physical_width as f32 / page_width;
        let render_cache = cx.global_mut::<RenderCache>();
        render_cache.render(previous_key).or_else(|| {
            render_cache.scaled_render(previous_doc_id, key.page_idx, key.rotation, render_scale)
        })
    }

    fn start_diff_flash_task(
        &mut self,
        input: DiffRenderInput,
        previous_image: Arc<RenderImage>,
        image: Arc<RenderImage>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let rects = cx
                .background_spawn(async move {
                    Self::diff_render_images(
                        &previous_image,
                        &image,
                        Rect::new(0.0, 0.0, input.page_width, input.page_height),
                        input.page_width,
                        input.page_height,
                        input.key.physical_width,
                        input.key.physical_height,
                    )
                })
                .await;

            if rects.is_empty() {
                return;
            }

            let update_result = this.update(cx, |this, cx| {
                let started_at = Instant::now();
                this.diff_flashes.extend(rects.into_iter().map(|rect| {
                    let rect = Self::view_rect_to_pdf(input.page_bounds, input.rotation, rect);
                    DiffFlash {
                        page_idx: input.key.page_idx,
                        rect,
                        started_at,
                    }
                }));
                this.arm_diff_flash_clear(cx);
                cx.notify();
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to show PDF reload diff flash: {error:#}");
            }
        })
        .detach();
    }

    #[ztracing::instrument(skip_all, fields(physical_width, physical_height))]
    fn render_page(
        display_list: &DisplayList,
        page_bounds: Rect,
        source_page_width: f32,
        source_page_height: f32,
        rotation: DocumentRotation,
        physical_width: u32,
        physical_height: u32,
    ) -> Result<Arc<RenderImage>> {
        let (page_width, page_height) =
            Self::rotated_dimensions(source_page_width, source_page_height, rotation);
        let zoom_x = physical_width as f32 / page_width;
        let zoom_y = physical_height as f32 / page_height;
        let physical_width = physical_width as i32;
        let physical_height = physical_height as i32;

        let mut pixmap = Pixmap::new_with_w_h(
            &Colorspace::device_rgb(),
            physical_width,
            physical_height,
            true,
        )?;
        pixmap.samples_mut().fill(255);

        let matrix = Self::render_matrix(
            page_bounds,
            rotation,
            zoom_x,
            zoom_y,
            mupdf::Point { x: 0.0, y: 0.0 },
        );
        {
            let device = Device::from_pixmap(&pixmap)?;
            display_list
                .run(&device, &matrix, Rect::INF)
                .context("failed to render PDF page")?;
        }

        let width = pixmap.width();
        let height = pixmap.height();
        let samples = pixmap.samples();
        let expected_len = width as usize * height as usize * 4;
        if samples.len() != expected_len {
            bail!(
                "unexpected pixmap sample length: got {}, expected {}",
                samples.len(),
                expected_len
            );
        }

        let mut bgra = Vec::with_capacity(samples.len());
        for pixel in samples.chunks_exact(4) {
            bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }

        let frame = Frame::new(
            RgbaImage::from_raw(width, height, bgra)
                .context("failed to create render image from pixmap")?,
        );

        Ok(Arc::new(RenderImage::new(smallvec![frame])))
    }

    fn arm_diff_flash_clear(&mut self, cx: &mut Context<Self>) {
        self.diff_flash_clear_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DIFF_FLASH_DURATION).await;
            let update_result = this.update(cx, |this, cx| {
                this.diff_flashes.clear();
                this.diff_flash_clear_task = None;
                cx.notify();
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to clear PDF reload diff flash: {error:#}");
            }
        }));
    }

    fn diff_render_images(
        previous_image: &RenderImage,
        image: &RenderImage,
        page_bounds: Rect,
        page_width: f32,
        page_height: f32,
        physical_width: u32,
        physical_height: u32,
    ) -> Vec<Rect> {
        if physical_width == 0 || physical_height == 0 {
            return Vec::new();
        }

        let Some(expected_len) = (physical_width as usize)
            .checked_mul(physical_height as usize)
            .and_then(|len| len.checked_mul(4))
        else {
            return Vec::new();
        };
        let Some(previous_bytes) = previous_image.as_bytes(0) else {
            return Vec::new();
        };
        let Some(bytes) = image.as_bytes(0) else {
            return Vec::new();
        };
        if previous_bytes.len() != expected_len || bytes.len() != expected_len {
            return Vec::new();
        }

        let tile_size = DIFF_FLASH_TILE_SIZE.max(1);
        let cols = physical_width.div_ceil(tile_size);
        let rows = physical_height.div_ceil(tile_size);
        let Some(cell_count) = (cols as usize).checked_mul(rows as usize) else {
            return Vec::new();
        };
        let mut changed_cells = vec![false; cell_count];

        for row in 0..rows {
            for col in 0..cols {
                let x0 = col * tile_size;
                let y0 = row * tile_size;
                let x1 = ((col + 1) * tile_size).min(physical_width);
                let y1 = ((row + 1) * tile_size).min(physical_height);
                let area = (x1 - x0).saturating_mul(y1 - y0);
                let changed_threshold = (area / 100).max(2);
                let mut changed_pixels = 0_u32;

                'tile: for y in y0..y1 {
                    for x in x0..x1 {
                        let offset = ((y as usize * physical_width as usize) + x as usize) * 4;
                        let delta = u16::from(previous_bytes[offset].abs_diff(bytes[offset]))
                            + u16::from(previous_bytes[offset + 1].abs_diff(bytes[offset + 1]))
                            + u16::from(previous_bytes[offset + 2].abs_diff(bytes[offset + 2]));
                        if delta > DIFF_FLASH_PIXEL_THRESHOLD {
                            changed_pixels += 1;
                            if changed_pixels >= changed_threshold {
                                changed_cells[(row as usize * cols as usize) + col as usize] = true;
                                break 'tile;
                            }
                        }
                    }
                }
            }
        }

        let mut runs = Vec::new();
        for row in 0..rows {
            let mut col = 0;
            while col < cols {
                if !changed_cells[(row as usize * cols as usize) + col as usize] {
                    col += 1;
                    continue;
                }

                let start_col = col;
                while col < cols && changed_cells[(row as usize * cols as usize) + col as usize] {
                    col += 1;
                }
                let end_col = col;

                if let Some((last_x0, _, last_x1, last_y1)) = runs.last_mut()
                    && *last_x0 == start_col
                    && *last_x1 == end_col
                    && *last_y1 == row
                {
                    *last_y1 = row + 1;
                } else {
                    runs.push((start_col, row, end_col, row + 1));
                }
            }
        }

        if runs.len() > DIFF_FLASH_MAX_RECTS {
            let mut min_x = u32::MAX;
            let mut min_y = u32::MAX;
            let mut max_x = 0;
            let mut max_y = 0;
            for (x0, y0, x1, y1) in runs {
                min_x = min_x.min(x0);
                min_y = min_y.min(y0);
                max_x = max_x.max(x1);
                max_y = max_y.max(y1);
            }
            runs = vec![(min_x, min_y, max_x, max_y)];
        }

        let scale_x = page_width / physical_width as f32;
        let scale_y = page_height / physical_height as f32;
        runs.into_iter()
            .filter_map(|(cell_x0, cell_y0, cell_x1, cell_y1)| {
                let pixel_x0 = (cell_x0 * tile_size).min(physical_width);
                let pixel_y0 = (cell_y0 * tile_size).min(physical_height);
                let pixel_x1 = (cell_x1 * tile_size).min(physical_width);
                let pixel_y1 = (cell_y1 * tile_size).min(physical_height);
                if pixel_x1 <= pixel_x0 || pixel_y1 <= pixel_y0 {
                    return None;
                }

                Some(Rect::new(
                    page_bounds.x0 + pixel_x0 as f32 * scale_x,
                    page_bounds.y0 + pixel_y0 as f32 * scale_y,
                    page_bounds.x0 + pixel_x1 as f32 * scale_x,
                    page_bounds.y0 + pixel_y1 as f32 * scale_y,
                ))
            })
            .collect()
    }

    fn collect_images(display_list: &DisplayList) -> Result<Vec<CollectedImage>> {
        let collector = Rc::new(RefCell::new(ImageCollector::default()));
        let device = Device::from_native(collector.clone())?;
        display_list
            .run(&device, &Matrix::IDENTITY, Rect::INF)
            .context("failed to collect PDF images")?;
        drop(device);

        Ok(std::mem::take(&mut collector.borrow_mut().images))
    }

    fn image_at_screen(
        &mut self,
        point: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<(usize, usize)> {
        let page_idx = self.page_at_screen(point)?;
        let pdf_point = self.screen_to_pdf(page_idx, point)?;
        let display_list = match self.get_or_create_display_list(page_idx, cx) {
            Ok(display_list) => display_list,
            Err(error) => {
                eprintln!("failed to load PDF page for image hit-test: {error:#}");
                return None;
            }
        };
        let images = match Self::collect_images(&display_list) {
            Ok(images) => images,
            Err(error) => {
                eprintln!("failed to hit-test PDF images: {error:#}");
                return None;
            }
        };

        images
            .iter()
            .enumerate()
            .rev()
            .find_map(|(image_idx, image)| {
                Self::image_contains_pdf_point(image, pdf_point).then_some((page_idx, image_idx))
            })
    }

    fn link_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        let page_idx = self.page_at_screen(point)?;
        let pdf_point = self.screen_to_pdf(page_idx, point)?;

        self.links.iter().enumerate().find_map(|(link_ix, link)| {
            (link.page_idx == page_idx
                && link
                    .rects
                    .iter()
                    .any(|rect| rect.contains(pdf_point.x, pdf_point.y)))
            .then_some(link_ix)
        })
    }

    fn highlight_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<u64> {
        let page_idx = self.page_at_screen(point)?;
        let pdf_point = self.screen_to_pdf(page_idx, point)?;

        self.highlights.iter().rev().find_map(|highlight| {
            highlight
                .rects
                .iter()
                .any(|rect| rect.contains(page_idx, pdf_point.x, pdf_point.y))
                .then_some(highlight.id)
        })
    }

    fn highlight_color_for_id(&self, id: u64) -> Option<HighlightColor> {
        self.highlights
            .iter()
            .find(|highlight| highlight.id == id)
            .map(|highlight| highlight.color)
    }

    fn note_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<u64> {
        let page_idx = self.page_at_screen(point)?;
        let pdf_point = self.screen_to_pdf(page_idx, point)?;

        self.notes.iter().rev().find_map(|note| {
            note.rects
                .iter()
                .any(|rect| rect.contains(page_idx, pdf_point.x, pdf_point.y))
                .then_some(note.id)
        })
    }

    fn note_body_for_id(&self, id: u64) -> Option<SharedString> {
        self.notes
            .iter()
            .find(|note| note.id == id)
            .map(|note| note.body.clone().into())
    }

    fn set_hovered_link(&mut self, link_ix: Option<usize>, cx: &mut Context<Self>) {
        if self.hovered_link == link_ix {
            return;
        }

        self.hovered_link = link_ix;
        cx.notify();
    }

    fn set_hovered_note(
        &mut self,
        note_id: Option<u64>,
        position: Option<GpuiPoint<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if self.hovered_note == note_id && self.hovered_note_position == position {
            return;
        }

        self.hovered_note = note_id;
        self.hovered_note_position = position;
        cx.notify();
    }

    fn update_hovered_link(&mut self, position: GpuiPoint<Pixels>, cx: &mut Context<Self>) {
        let link_ix = if self.dragging || self.panning.is_some() {
            None
        } else {
            self.link_at_screen(position)
        };
        self.set_hovered_link(link_ix, cx);
    }

    fn update_hovered_note(&mut self, position: GpuiPoint<Pixels>, cx: &mut Context<Self>) {
        let note_id = if self.dragging || self.panning.is_some() {
            None
        } else {
            self.note_at_screen(position)
        };
        let position = note_id.map(|_| position);
        self.set_hovered_note(note_id, position, cx);
    }

    fn image_contains_pdf_point(image: &CollectedImage, point: mupdf::Point) -> bool {
        if !image.bounds.contains(point.x, point.y) {
            return false;
        }

        let Some(inverse) = image.matrix.invert() else {
            return true;
        };

        let (x, y) = inverse.transform_xy(point.x, point.y);
        (-0.001..=1.001).contains(&x) && (-0.001..=1.001).contains(&y)
    }

    fn copy_image(&mut self, cx: &mut Context<Self>) {
        let Some((page_idx, image_idx)) = self.context_menu_image else {
            return;
        };
        if page_idx >= self.pages.len() {
            return;
        };
        let display_list = match self.get_or_create_display_list(page_idx, cx) {
            Ok(display_list) => display_list,
            Err(error) => {
                eprintln!("failed to load PDF page for image copy: {error:#}");
                return;
            }
        };

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { Self::copy_image_to_clipboard(display_list, image_idx) },
                )
                .await;

            if let Err(error) = this.update(cx, |this, _| match result {
                Ok(clipboard) => {
                    this._image_clipboard = Some(clipboard);
                }
                Err(error) => eprintln!("failed to copy PDF image: {error:#}"),
            }) {
                eprintln!("failed to update PDF image clipboard state: {error:#}");
            }
        })
        .detach();
    }

    fn copy_image_to_clipboard(
        display_list: Arc<DisplayList>,
        image_idx: usize,
    ) -> Result<Clipboard> {
        let (width, height, rgba) =
            Self::rasterize_image_from_display_list(&display_list, image_idx)?;

        let mut clipboard = Clipboard::new().context("failed to open clipboard")?;
        clipboard
            .set_image(ImageData {
                width,
                height,
                bytes: Cow::Owned(rgba),
            })
            .context("failed to write image to clipboard")?;

        Ok(clipboard)
    }

    fn save_image(&mut self, cx: &mut Context<Self>) {
        let Some((page_idx, image_idx)) = self.context_menu_image else {
            return;
        };
        if page_idx >= self.pages.len() {
            return;
        };
        let display_list = match self.get_or_create_display_list(page_idx, cx) {
            Ok(display_list) => display_list,
            Err(error) => {
                eprintln!("failed to load PDF page for image save: {error:#}");
                return;
            }
        };
        let suggested_name = format!("page-{}-image-{}.png", page_idx + 1, image_idx + 1);
        let directory = dirs::picture_dir()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |_, cx| match dialog.await {
            Ok(Ok(Some(path))) => {
                let result = cx
                    .background_spawn(async move {
                        Self::save_image_to_path(display_list, image_idx, path)
                    })
                    .await;

                if let Err(error) = result {
                    eprintln!("failed to save PDF image: {error:#}");
                }
            }
            Ok(Err(error)) => eprintln!("failed to open save image dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();
    }

    fn save_image_to_path(
        display_list: Arc<DisplayList>,
        image_idx: usize,
        path: PathBuf,
    ) -> Result<()> {
        let (width, height, rgba) =
            Self::rasterize_image_from_display_list(&display_list, image_idx)?;
        let image = RgbaImage::from_raw(width as u32, height as u32, rgba)
            .context("failed to create image data")?;
        image
            .save_with_format(&path, image::ImageFormat::Png)
            .with_context(|| format!("failed to save image to {}", path.display()))
    }

    fn save_page_as_image(&mut self, cx: &mut Context<Self>) {
        let Some(page_idx) = self.context_menu_page else {
            return;
        };
        let Some(page) = self.pages.get(page_idx) else {
            return;
        };
        let page_bounds = page.bounds;
        let page_width = page.width;
        let page_height = page.height;
        let rotation = self.rotation;
        let display_list = match self.get_or_create_display_list(page_idx, cx) {
            Ok(display_list) => display_list,
            Err(error) => {
                eprintln!("failed to load PDF page for image export: {error:#}");
                return;
            }
        };
        let suggested_name = format!("page-{}.png", page_idx + 1);
        let directory = dirs::picture_dir()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |_, cx| match dialog.await {
            Ok(Ok(Some(path))) => {
                let result = cx
                    .background_spawn(async move {
                        Self::save_page_to_path(
                            display_list,
                            page_bounds,
                            page_width,
                            page_height,
                            rotation,
                            path,
                        )
                    })
                    .await;

                if let Err(error) = result {
                    eprintln!("failed to save PDF page as image: {error:#}");
                }
            }
            Ok(Err(error)) => eprintln!("failed to open save page image dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();
    }

    fn region_image_exports(
        &mut self,
        reference: &RegionImageReference,
        cx: &mut Context<Self>,
    ) -> Result<Vec<RegionImageExport>> {
        reference
            .regions
            .iter()
            .map(|region| {
                let page_idx = region.page.0;
                if page_idx >= self.pages.len() {
                    bail!("selected region references a missing PDF page");
                }
                Ok(RegionImageExport {
                    display_list: self.get_or_create_display_list(page_idx, cx)?,
                    page_bounds: self.pages[page_idx].bounds,
                    region: *region,
                    rotation: self.rotation,
                })
            })
            .collect()
    }

    fn copy_region(&mut self, cx: &mut Context<Self>) {
        let Some(reference) = self.selected_region.clone() else {
            return;
        };
        let exports = match self.region_image_exports(&reference, cx) {
            Ok(exports) => exports,
            Err(error) => {
                eprintln!("failed to prepare PDF region copy: {error:#}");
                return;
            }
        };

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let (width, height, rgba) =
                        Self::rasterize_region_stack_to_rgba(&exports, PAGE_EXPORT_SCALE)?;
                    let mut clipboard =
                        Clipboard::new().context("failed to open clipboard for region copy")?;
                    clipboard
                        .set_image(ImageData {
                            width: width as usize,
                            height: height as usize,
                            bytes: Cow::Owned(rgba),
                        })
                        .context("failed to write PDF region image to clipboard")?;
                    Ok::<_, anyhow::Error>(clipboard)
                })
                .await;

            if let Err(error) = this.update(cx, |this, cx| match result {
                Ok(clipboard) => {
                    this._image_clipboard = Some(clipboard);
                    if clear_copied_region_selection(
                        &mut this.selected_region,
                        &mut this.context_menu_region,
                        &reference,
                    ) {
                        cx.notify();
                    }
                }
                Err(error) => eprintln!("failed to copy PDF region: {error:#}"),
            }) {
                eprintln!("failed to update PDF region clipboard state: {error:#}");
            }
        })
        .detach();
    }

    fn save_region(&mut self, cx: &mut Context<Self>) {
        let Some(reference) = self.selected_region.clone() else {
            return;
        };
        let exports = match self.region_image_exports(&reference, cx) {
            Ok(exports) => exports,
            Err(error) => {
                eprintln!("failed to prepare PDF region save: {error:#}");
                return;
            }
        };
        let first_page = reference
            .regions
            .first()
            .map_or(1, |region| region.page.0 + 1);
        let last_page = reference
            .regions
            .last()
            .map_or(first_page, |region| region.page.0 + 1);
        let suggested_name = if first_page == last_page {
            format!("region-page-{first_page}.png")
        } else {
            format!("region-pages-{first_page}-{last_page}.png")
        };
        let directory = dirs::picture_dir()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |_, cx| match dialog.await {
            Ok(Ok(Some(path))) => {
                let result = cx
                    .background_spawn(async move {
                        let (width, height, rgba) =
                            Self::rasterize_region_stack_to_rgba(&exports, PAGE_EXPORT_SCALE)?;
                        let image = RgbaImage::from_raw(width, height, rgba)
                            .context("failed to create region image data")?;
                        image
                            .save_with_format(&path, image::ImageFormat::Png)
                            .with_context(|| {
                                format!("failed to save region image to {}", path.display())
                            })
                    })
                    .await;
                if let Err(error) = result {
                    eprintln!("failed to save PDF region: {error:#}");
                }
            }
            Ok(Err(error)) => eprintln!("failed to open save region dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();
    }

    fn rasterize_region_stack_to_rgba(
        exports: &[RegionImageExport],
        scale: f32,
    ) -> Result<(u32, u32, Vec<u8>)> {
        if exports.is_empty() || !scale.is_finite() || scale <= 0.0 {
            bail!("PDF image region is empty");
        }

        let mut dimensions = Vec::with_capacity(exports.len());
        let mut max_width = 1u32;
        let mut total_height = 0u32;
        for export in exports {
            let view_region = export.view_region();
            let width = ((view_region.x1 - view_region.x0) * scale).ceil().max(1.0);
            let height = ((view_region.y1 - view_region.y0) * scale).ceil().max(1.0);
            if width > i32::MAX as f32 || height > i32::MAX as f32 {
                bail!("PDF image region is too large");
            }
            let width = width as u32;
            let height = height as u32;
            max_width = max_width.max(width);
            total_height = total_height
                .checked_add(height)
                .context("combined region image is too tall")?;
            dimensions.push((width, height));
        }

        let total_pixels = (max_width as u64)
            .checked_mul(total_height as u64)
            .context("combined region image is too large")?;
        if total_pixels > MAX_REGION_EXPORT_PIXELS {
            bail!(
                "combined region image is too large: {} megapixels exceeds the {} megapixel limit",
                total_pixels.div_ceil(1_000_000),
                MAX_REGION_EXPORT_PIXELS / 1_000_000
            );
        }

        let mut image =
            RgbaImage::from_pixel(max_width, total_height, image::Rgba([255, 255, 255, 255]));
        let mut y_offset = 0;
        for (export, (width, height)) in exports.iter().zip(dimensions) {
            let (_, _, rgba) = Self::rasterize_pdf_region_to_rgba(export, scale)?;
            let crop = RgbaImage::from_raw(width, height, rgba)
                .context("failed to create region crop image data")?;
            let x_offset = (max_width - width) / 2;
            for y in 0..height {
                for x in 0..width {
                    image.put_pixel(x_offset + x, y_offset + y, *crop.get_pixel(x, y));
                }
            }
            y_offset = y_offset
                .checked_add(height)
                .context("combined region image is too tall")?;
        }

        Ok((max_width, total_height, image.into_raw()))
    }

    fn rasterize_pdf_region_to_rgba(
        export: &RegionImageExport,
        scale: f32,
    ) -> Result<(u32, u32, Vec<u8>)> {
        let view_region = export.view_region();
        let width = ((view_region.x1 - view_region.x0) * scale).ceil().max(1.0);
        let height = ((view_region.y1 - view_region.y0) * scale).ceil().max(1.0);
        if width > i32::MAX as f32 || height > i32::MAX as f32 {
            bail!("PDF image region is too large");
        }
        let width = width as i32;
        let height = height as i32;
        let mut pixmap = Pixmap::new_with_w_h(&Colorspace::device_rgb(), width, height, true)?;
        pixmap.samples_mut().fill(255);
        let matrix = Self::render_matrix(
            export.page_bounds,
            export.rotation,
            scale,
            scale,
            mupdf::Point {
                x: view_region.x0,
                y: view_region.y0,
            },
        );
        {
            let device = Device::from_pixmap(&pixmap)?;
            export
                .display_list
                .run(&device, &matrix, Rect::INF)
                .context("failed to rasterize PDF image region")?;
        }
        let width = pixmap.width();
        let height = pixmap.height();
        let samples = pixmap.samples();
        let expected_len = width as usize * height as usize * 4;
        if samples.len() != expected_len || pixmap.n() != 4 {
            bail!(
                "unexpected region image pixmap layout: {} samples, {} components",
                samples.len(),
                pixmap.n()
            );
        }
        Ok((width, height, samples.to_vec()))
    }

    fn render_region_jpeg(
        exports: &[RegionImageExport],
        quality: PageImageQuality,
    ) -> Result<Vec<u8>> {
        if exports.is_empty() {
            bail!("PDF image region is empty");
        }
        let max_width = exports
            .iter()
            .map(RegionImageExport::view_region)
            .map(|rect| rect.x1 - rect.x0)
            .fold(0.0_f32, f32::max);
        let total_height = exports
            .iter()
            .map(RegionImageExport::view_region)
            .map(|rect| rect.y1 - rect.y0)
            .sum::<f32>();
        let scale = quality.max_dimension() as f32 / max_width.max(total_height).max(1.0);
        let (width, height, rgba) = Self::rasterize_region_stack_to_rgba(exports, scale)?;
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for pixel in rgba.chunks_exact(4) {
            rgb.extend_from_slice(&pixel[..3]);
        }
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, quality.jpeg_quality())
            .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
            .context("failed to encode PDF image region as JPEG")?;
        Ok(bytes)
    }

    fn export_selected_pages_as_text(
        &mut self,
        source: PageSelectionSource,
        cx: &mut Context<Self>,
    ) {
        let page_indices = self.selected_page_indices(source);
        if page_indices.is_empty() {
            return;
        }

        let mut export_pages = Vec::with_capacity(page_indices.len());
        for page_idx in page_indices.iter().copied() {
            let display_list = match self.get_or_create_display_list(page_idx, cx) {
                Ok(display_list) => display_list,
                Err(error) => {
                    eprintln!("failed to load PDF page for text export: {error:#}");
                    return;
                }
            };
            export_pages.push((page_idx, display_list));
        }

        let suggested_name = if export_pages.len() == 1 {
            format!("page-{}.txt", export_pages[0].0 + 1)
        } else {
            "selected-pages.txt".to_string()
        };
        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |_, cx| match dialog.await {
            Ok(Ok(Some(path))) => {
                let result = cx
                    .background_spawn(async move {
                        Self::save_selected_pages_text_to_path(export_pages, path)
                    })
                    .await;

                if let Err(error) = result {
                    eprintln!("failed to export selected PDF pages as text: {error:#}");
                }
            }
            Ok(Err(error)) => eprintln!("failed to open text export dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();

        self.cancel_page_selection(source, cx);
    }

    fn export_selected_pages_as_image(
        &mut self,
        source: PageSelectionSource,
        cx: &mut Context<Self>,
    ) {
        let page_indices = self.selected_page_indices(source);
        if page_indices.is_empty() {
            return;
        }

        let mut export_pages = Vec::with_capacity(page_indices.len());
        let rotation = self.rotation;
        for page_idx in page_indices {
            let Some(page) = self.pages.get(page_idx) else {
                continue;
            };
            let bounds = page.bounds;
            let width = page.width;
            let height = page.height;
            let display_list = match self.get_or_create_display_list(page_idx, cx) {
                Ok(display_list) => display_list,
                Err(error) => {
                    eprintln!("failed to load PDF page for image export: {error:#}");
                    return;
                }
            };
            export_pages.push(PageImageExport {
                page_idx,
                display_list,
                bounds,
                width,
                height,
                rotation,
            });
        }

        if export_pages.is_empty() {
            return;
        }

        let suggested_name = if export_pages.len() == 1 {
            format!("page-{}.png", export_pages[0].page_idx + 1)
        } else {
            "selected-pages.png".to_string()
        };
        let directory = dirs::picture_dir()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |_, cx| match dialog.await {
            Ok(Ok(Some(path))) => {
                let result = cx
                    .background_spawn(async move {
                        Self::save_selected_pages_image_to_path(export_pages, path)
                    })
                    .await;

                if let Err(error) = result {
                    eprintln!("failed to export selected PDF pages as image: {error:#}");
                }
            }
            Ok(Err(error)) => eprintln!("failed to open image export dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();

        self.cancel_page_selection(source, cx);
    }

    fn save_selected_pages_text_to_path(
        pages: Vec<(usize, Arc<DisplayList>)>,
        path: PathBuf,
    ) -> Result<()> {
        let mut text = String::new();
        for (page_ix, (_, display_list)) in pages.into_iter().enumerate() {
            if page_ix > 0 {
                text.push_str("\n\n");
            }
            text.push_str(&Self::text_from_display_list(display_list.as_ref())?);
        }

        fs::write(&path, text).with_context(|| format!("failed to save text to {}", path.display()))
    }

    fn text_from_display_list(display_list: &DisplayList) -> Result<String> {
        let mut glyphs = Vec::new();
        let mut text_lines = Vec::new();
        let mut next_line_idx = 0;
        let mut next_block_idx = 0;
        let range = Self::extract_glyphs(
            display_list,
            0,
            &mut glyphs,
            &mut text_lines,
            &mut next_line_idx,
            &mut next_block_idx,
        )?;
        Ok(Self::text_for_glyph_range(&glyphs, range).unwrap_or_default())
    }

    fn save_selected_pages_image_to_path(pages: Vec<PageImageExport>, path: PathBuf) -> Result<()> {
        if pages.is_empty() {
            bail!("no pages selected for image export");
        }

        let mut rendered_pages = Vec::with_capacity(pages.len());
        let mut max_width = 1u32;
        let mut total_height = 0u32;

        for page in pages {
            let (width, height, rgba) = Self::rasterize_page_to_rgba(
                page.display_list.as_ref(),
                page.bounds,
                page.width,
                page.height,
                page.rotation,
            )?;
            max_width = max_width.max(width);
            total_height = total_height
                .checked_add(height)
                .context("combined page image is too tall")?;
            rendered_pages.push((width, height, rgba));
        }

        let total_bytes = (max_width as u64)
            .checked_mul(total_height as u64)
            .and_then(|pixels| pixels.checked_mul(4))
            .context("combined page image is too large")?;
        if total_bytes > usize::MAX as u64 {
            bail!("combined page image is too large");
        }

        let mut image =
            RgbaImage::from_pixel(max_width, total_height, image::Rgba([255, 255, 255, 255]));
        let mut y_offset = 0u32;
        for (width, height, rgba) in rendered_pages {
            let page_image = RgbaImage::from_raw(width, height, rgba)
                .context("failed to create page image data")?;
            let x_offset = (max_width - width) / 2;
            for y in 0..height {
                for x in 0..width {
                    let pixel = *page_image.get_pixel(x, y);
                    image.put_pixel(x_offset + x, y_offset + y, pixel);
                }
            }
            y_offset = y_offset
                .checked_add(height)
                .context("combined page image is too tall")?;
        }

        image
            .save_with_format(&path, image::ImageFormat::Png)
            .with_context(|| format!("failed to save image to {}", path.display()))
    }

    fn save_page_to_path(
        display_list: Arc<DisplayList>,
        page_bounds: Rect,
        page_width: f32,
        page_height: f32,
        rotation: DocumentRotation,
        path: PathBuf,
    ) -> Result<()> {
        let (width, height, rgba) = Self::rasterize_page_to_rgba(
            &display_list,
            page_bounds,
            page_width,
            page_height,
            rotation,
        )?;
        let image =
            RgbaImage::from_raw(width, height, rgba).context("failed to create page image data")?;
        image
            .save_with_format(&path, image::ImageFormat::Png)
            .with_context(|| format!("failed to save image to {}", path.display()))
    }

    #[ztracing::instrument(skip_all)]
    fn rasterize_page_to_rgba(
        display_list: &DisplayList,
        page_bounds: Rect,
        page_width: f32,
        page_height: f32,
        rotation: DocumentRotation,
    ) -> Result<(u32, u32, Vec<u8>)> {
        let (display_width, display_height) =
            Self::rotated_dimensions(page_width, page_height, rotation);
        let physical_width = (display_width * PAGE_EXPORT_SCALE).ceil().max(1.0);
        let physical_height = (display_height * PAGE_EXPORT_SCALE).ceil().max(1.0);
        if physical_width > i32::MAX as f32 || physical_height > i32::MAX as f32 {
            bail!("PDF page is too large to export as an image");
        }

        let physical_width_i32 = physical_width as i32;
        let physical_height_i32 = physical_height as i32;
        let mut pixmap = Pixmap::new_with_w_h(
            &Colorspace::device_rgb(),
            physical_width_i32,
            physical_height_i32,
            true,
        )?;
        pixmap.samples_mut().fill(255);

        let zoom_x = physical_width / display_width.max(1.0);
        let zoom_y = physical_height / display_height.max(1.0);
        let matrix = Self::render_matrix(
            page_bounds,
            rotation,
            zoom_x,
            zoom_y,
            mupdf::Point { x: 0.0, y: 0.0 },
        );
        {
            let device = Device::from_pixmap(&pixmap)?;
            display_list
                .run(&device, &matrix, Rect::INF)
                .context("failed to render PDF page image")?;
        }

        let width = pixmap.width();
        let height = pixmap.height();
        let samples = pixmap.samples();
        let expected_len = width as usize * height as usize * 4;
        if samples.len() != expected_len || pixmap.n() != 4 {
            bail!(
                "unexpected page image pixmap layout: {} samples, {} components",
                samples.len(),
                pixmap.n()
            );
        }

        Ok((width, height, samples.to_vec()))
    }

    fn rasterize_image_from_display_list(
        display_list: &DisplayList,
        image_idx: usize,
    ) -> Result<(usize, usize, Vec<u8>)> {
        let image = Self::collect_images(display_list)?
            .into_iter()
            .nth(image_idx)
            .ok_or_else(|| anyhow!("PDF image no longer exists"))?;

        Self::rasterize_image_to_rgba(&image.image)
    }

    fn rasterize_image_to_rgba(image: &PdfImage) -> Result<(usize, usize, Vec<u8>)> {
        let width = image.width();
        let height = image.height();
        if width == 0 || height == 0 {
            bail!("PDF image has zero dimensions");
        }
        if width > i32::MAX as u32 || height > i32::MAX as u32 {
            bail!("PDF image is too large to copy");
        }

        let mut pixmap =
            Pixmap::new_with_w_h(&Colorspace::device_rgb(), width as i32, height as i32, true)?;
        pixmap.clear()?;

        let matrix = Matrix::new(width as f32, 0.0, 0.0, height as f32, 0.0, 0.0);
        {
            let device = Device::from_pixmap(&pixmap)?;
            device
                .fill_image(image, &matrix, 1.0, ColorParams::default())
                .context("failed to rasterize PDF image")?;
        }

        let samples = pixmap.samples();
        let expected_len = width as usize * height as usize * 4;
        if samples.len() != expected_len || pixmap.n() != 4 {
            bail!(
                "unexpected image pixmap layout: {} samples, {} components",
                samples.len(),
                pixmap.n()
            );
        }

        let mut rgba = samples.to_vec();
        Self::unpremultiply_rgba(&mut rgba);
        Ok((width as usize, height as usize, rgba))
    }

    fn unpremultiply_rgba(rgba: &mut [u8]) {
        for pixel in rgba.chunks_exact_mut(4) {
            let alpha = pixel[3] as u32;
            if alpha == 0 {
                pixel[0] = 0;
                pixel[1] = 0;
                pixel[2] = 0;
            } else if alpha < 255 {
                pixel[0] = ((pixel[0] as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                pixel[1] = ((pixel[1] as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                pixel[2] = ((pixel[2] as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }

    fn screen_to_document(&self, point: GpuiPoint<Pixels>) -> Option<mupdf::Point> {
        let (page_idx, bounds) = self
            .page_image_bounds
            .iter()
            .enumerate()
            .find_map(|(page_idx, bounds)| bounds.as_ref().map(|bounds| (page_idx, bounds)))?;
        let page = self.pages.get(page_idx)?;
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let scale_x = bounds.size.width.as_f32() / page_width;
        let scale_y = bounds.size.height.as_f32() / page_height;
        if scale_x <= 0.0 || scale_y <= 0.0 {
            return None;
        }

        let page_left = (self.max_page_width - page_width) / 2.0;
        Some(mupdf::Point {
            x: (page_left + (point.x.as_f32() - bounds.origin.x.as_f32()) / scale_x)
                .clamp(0.0, self.max_page_width),
            y: (page.offset_y + (point.y.as_f32() - bounds.origin.y.as_f32()) / scale_y)
                .clamp(0.0, self.content_height_doc),
        })
    }

    fn region_reference_for_drag(&self, drag: RegionDragState) -> Option<RegionImageReference> {
        Self::region_reference_for_pages(
            &self.pages,
            self.max_page_width,
            self.effective_scale(),
            self.rotation,
            drag,
        )
    }

    fn region_reference_for_pages(
        pages: &[PdfPage],
        max_page_width: f32,
        scale: f32,
        rotation: DocumentRotation,
        drag: RegionDragState,
    ) -> Option<RegionImageReference> {
        let x0 = drag.anchor.x.min(drag.current.x);
        let y0 = drag.anchor.y.min(drag.current.y);
        let x1 = drag.anchor.x.max(drag.current.x);
        let y1 = drag.anchor.y.max(drag.current.y);
        if (x1 - x0) * scale < MIN_REGION_SELECTION_PX
            || (y1 - y0) * scale < MIN_REGION_SELECTION_PX
        {
            return None;
        }

        let display_height = |page: &PdfPage| Self::page_dimensions_for(page, rotation).1;
        let start_page = pages.partition_point(|page| page.offset_y + display_height(page) <= y0);
        let end_page = start_page + pages[start_page..].partition_point(|page| page.offset_y < y1);
        let regions = pages[start_page..end_page]
            .iter()
            .enumerate()
            .filter_map(|(relative_page_idx, page)| {
                let (page_width, page_height) = Self::page_dimensions_for(page, rotation);
                let page_left = (max_page_width - page_width) / 2.0;
                let page_right = page_left + page_width;
                let page_bottom = page.offset_y + page_height;
                let region_x0 = x0.max(page_left);
                let region_y0 = y0.max(page.offset_y);
                let region_x1 = x1.min(page_right);
                let region_y1 = y1.min(page_bottom);
                if region_x1 <= region_x0 || region_y1 <= region_y0 {
                    return None;
                }

                let source_rect = Self::view_rect_to_pdf(
                    page.bounds,
                    rotation,
                    Rect::new(
                        region_x0 - page_left,
                        region_y0 - page.offset_y,
                        region_x1 - page_left,
                        region_y1 - page.offset_y,
                    ),
                );
                Some(PageRegion {
                    page: PageIndex(start_page + relative_page_idx),
                    x0: source_rect.x0,
                    y0: source_rect.y0,
                    x1: source_rect.x1,
                    y1: source_rect.y1,
                })
            })
            .collect::<Vec<_>>();
        let reference = RegionImageReference { regions };
        (!reference.is_empty()).then_some(reference)
    }

    fn active_region_reference(&self) -> Option<RegionImageReference> {
        self.region_drag
            .and_then(|drag| self.region_reference_for_drag(drag))
            .or_else(|| self.selected_region.clone())
    }

    fn region_overlay_bounds(&self, reference: &RegionImageReference) -> Vec<Bounds<Pixels>> {
        reference
            .regions
            .iter()
            .filter_map(|region| {
                let image_bounds = *self.page_image_bounds.get(region.page.0)?.as_ref()?;
                Some(self.pdf_rect_to_screen(
                    region.page.0,
                    Rect::new(region.x0, region.y0, region.x1, region.y1),
                    image_bounds,
                ))
            })
            .collect()
    }

    fn point_in_selected_region(&self, point: GpuiPoint<Pixels>) -> bool {
        self.selected_region.as_ref().is_some_and(|reference| {
            self.region_overlay_bounds(reference)
                .iter()
                .any(|bounds| bounds.contains(&point))
        })
    }

    fn region_popover_position(&self) -> Option<GpuiPoint<Pixels>> {
        self.selected_region
            .as_ref()
            .and_then(|reference| self.region_overlay_bounds(reference).into_iter().last())
            .map(|bounds| point(bounds.right(), bounds.bottom() + px(6.0)))
    }

    fn point_in_image(&self, point: GpuiPoint<Pixels>) -> bool {
        self.page_at_screen(point).is_some()
    }

    fn page_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        self.page_image_bounds
            .iter()
            .enumerate()
            .find_map(|(page_idx, bounds)| {
                let bounds = bounds.as_ref()?;
                let x = point.x.as_f32();
                let y = point.y.as_f32();
                let x0 = bounds.origin.x.as_f32();
                let y0 = bounds.origin.y.as_f32();
                let x1 = x0 + bounds.size.width.as_f32();
                let y1 = y0 + bounds.size.height.as_f32();

                (x >= x0 && x <= x1 && y >= y0 && y <= y1).then_some(page_idx)
            })
    }

    fn screen_to_pdf(&self, page_idx: usize, point: GpuiPoint<Pixels>) -> Option<mupdf::Point> {
        let bounds = self.page_image_bounds.get(page_idx)?.as_ref()?;
        let page = &self.pages[page_idx];
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let scale_x = bounds.size.width.as_f32() / page_width;
        let scale_y = bounds.size.height.as_f32() / page_height;
        if scale_x <= 0.0 || scale_y <= 0.0 {
            return None;
        }

        Some(Self::view_point_to_pdf(
            page.bounds,
            self.rotation,
            mupdf::Point {
                x: (point.x.as_f32() - bounds.origin.x.as_f32()) / scale_x,
                y: (point.y.as_f32() - bounds.origin.y.as_f32()) / scale_y,
            },
        ))
    }

    fn glyph_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        let page_idx = self.page_at_screen(point)?;
        self.screen_to_pdf(page_idx, point)
            .and_then(|point| self.glyph_at_pdf(page_idx, point))
    }

    fn word_bounds_at_glyph(&self, ix: usize) -> Option<Range<usize>> {
        Self::word_bounds_in_glyphs(&self.glyphs, ix)
    }

    fn word_bounds_in_glyphs(glyphs: &[Glyph], ix: usize) -> Option<Range<usize>> {
        let glyph = glyphs.get(ix)?;
        if !Self::is_word_char(glyph.c) {
            return None;
        }

        let mut start = ix;
        while start > 0 {
            let previous = &glyphs[start - 1];
            if !Self::same_word_segment(previous, glyph)
                || !Self::is_word_char(previous.c)
                || Self::has_inferred_space_between(glyphs, start - 1, start)
            {
                break;
            }
            start -= 1;
        }

        let mut end = ix + 1;
        while end < glyphs.len() {
            let next = &glyphs[end];
            if !Self::same_word_segment(next, glyph)
                || !Self::is_word_char(next.c)
                || Self::has_inferred_space_between(glyphs, end - 1, end)
            {
                break;
            }
            end += 1;
        }

        Some(start..end)
    }

    #[cfg(test)]
    fn word_bounds_in_chars(chars: &[char], ix: usize) -> Option<Range<usize>> {
        if !Self::is_word_char(*chars.get(ix)?) {
            return None;
        }

        let mut start = ix;
        while start > 0 && Self::is_word_char(chars[start - 1]) {
            start -= 1;
        }

        let mut end = ix + 1;
        while end < chars.len() && Self::is_word_char(chars[end]) {
            end += 1;
        }

        Some(start..end)
    }

    fn same_word_segment(a: &Glyph, b: &Glyph) -> bool {
        a.page_idx == b.page_idx && a.block_idx == b.block_idx && a.line_idx == b.line_idx
    }

    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || matches!(c, '_' | '\'' | '’')
    }

    fn has_inferred_space_between(glyphs: &[Glyph], left_ix: usize, right_ix: usize) -> bool {
        let Some(left) = glyphs.get(left_ix) else {
            return false;
        };
        let Some(right) = glyphs.get(right_ix) else {
            return false;
        };

        Self::same_word_segment(left, right) && left.inferred_space_after
    }

    fn mark_inferred_spaces(glyphs: &mut [Glyph]) {
        let mut space_widths = glyphs
            .iter()
            .filter(|glyph| glyph.c.is_whitespace() && !glyph.c.is_control())
            .filter_map(|glyph| {
                let rect = Self::quad_rect(&glyph.quad);
                let width = rect.x1 - rect.x0;
                (width > 0.0).then_some(width)
            })
            .collect::<Vec<_>>();
        space_widths.sort_by(|left, right| left.total_cmp(right));
        let explicit_space_threshold = space_widths
            .get(space_widths.len() / 2)
            .map(|width| width * INFERRED_SPACE_WIDTH_RATIO);

        for index in 0..glyphs.len().saturating_sub(1) {
            let (left, right) = (&glyphs[index], &glyphs[index + 1]);
            if left.c.is_whitespace()
                || right.c.is_whitespace()
                || left.c.is_control()
                || right.c.is_control()
            {
                continue;
            }

            let left_rect = Self::quad_rect(&left.quad);
            let right_rect = Self::quad_rect(&right.quad);
            let gap = right_rect.x0 - left_rect.x1;
            if gap <= 0.0 {
                continue;
            }
            let fallback_threshold = (left_rect.y1 - left_rect.y0) * INFERRED_SPACE_HEIGHT_RATIO;
            if gap >= explicit_space_threshold.unwrap_or(fallback_threshold) {
                glyphs[index].inferred_space_after = true;
            }
        }
    }

    fn nearby_glyph_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        let page_idx = self.page_at_screen(point)?;
        let bounds = self.page_image_bounds.get(page_idx)?.as_ref()?;
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let scale = (bounds.size.width.as_f32() / page_width)
            .min(bounds.size.height.as_f32() / page_height);
        if scale <= 0.0 {
            return None;
        }

        let point = self.screen_to_pdf(page_idx, point)?;
        self.nearby_glyph_at_pdf(page_idx, point, INITIAL_SELECTION_TOLERANCE_PX / scale)
    }

    fn exact_glyph_at_pdf(&self, page_idx: usize, point: mupdf::Point) -> Option<usize> {
        let line_range = self.page_line_ranges.get(page_idx)?.clone();
        if line_range.start >= line_range.end || line_range.end > self.text_lines.len() {
            return None;
        }

        self.text_lines[line_range]
            .iter()
            .filter(|line| line.bounds.contains(point.x, point.y))
            .find_map(|line| {
                self.glyphs[line.glyph_range.clone()]
                    .iter()
                    .enumerate()
                    .find_map(|(offset, glyph)| {
                        Self::quad_rect(&glyph.quad)
                            .contains(point.x, point.y)
                            .then_some(line.glyph_range.start + offset)
                    })
            })
    }

    fn nearby_glyph_at_pdf(
        &self,
        page_idx: usize,
        point: mupdf::Point,
        tolerance: f32,
    ) -> Option<usize> {
        if let Some(ix) = self.exact_glyph_at_pdf(page_idx, point) {
            return Some(ix);
        }

        let line_range = self.page_line_ranges.get(page_idx)?.clone();
        if line_range.start >= line_range.end || line_range.end > self.text_lines.len() {
            return None;
        }

        self.text_lines[line_range]
            .iter()
            .flat_map(|line| {
                self.glyphs[line.glyph_range.clone()]
                    .iter()
                    .enumerate()
                    .map(move |(offset, glyph)| (line.glyph_range.start + offset, glyph))
            })
            .filter_map(|(glyph_ix, glyph)| {
                let distance = Self::distance_to_rect(point, Self::quad_rect(&glyph.quad));
                (distance <= tolerance).then_some((glyph_ix, distance))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(ix, _)| ix)
    }

    fn glyph_at_pdf(&self, page_idx: usize, point: mupdf::Point) -> Option<usize> {
        if self.pages.get(page_idx)?.glyph_range.is_empty() {
            return None;
        }

        if let Some(ix) = self.exact_glyph_at_pdf(page_idx, point) {
            return Some(ix);
        }

        let (_, start, end, line_rect) = self.nearest_line(page_idx, point)?;
        if point.x <= line_rect.x0 {
            return Some(start);
        }
        if point.x >= line_rect.x1 {
            return Some(end);
        }

        let mut nearest = start;
        let mut nearest_dist = f32::INFINITY;
        for ix in start..=end {
            let rect = Self::quad_rect(&self.glyphs[ix].quad);
            let center_x = (rect.x0 + rect.x1) / 2.0;
            let dist = (point.x - center_x).abs();
            if dist < nearest_dist {
                nearest = ix;
                nearest_dist = dist;
            }
        }

        Some(nearest)
    }

    fn nearest_line(
        &self,
        page_idx: usize,
        point: mupdf::Point,
    ) -> Option<(f32, usize, usize, Rect)> {
        let line_range = self.page_line_ranges.get(page_idx)?.clone();
        if line_range.start >= line_range.end || line_range.end > self.text_lines.len() {
            return None;
        }

        Self::nearest_line_in_slice(&self.text_lines[line_range], point)
    }

    fn nearest_line_in_slice(
        text_lines: &[TextLine],
        point: mupdf::Point,
    ) -> Option<(f32, usize, usize, Rect)> {
        text_lines
            .iter()
            .filter_map(|line| {
                let start = line.glyph_range.start;
                let end = line.glyph_range.end.checked_sub(1)?;
                let rect = line.bounds;
                Some((
                    Self::squared_distance_to_rect(point, rect),
                    start,
                    end,
                    rect,
                ))
            })
            .min_by(|left, right| {
                left.0
                    .total_cmp(&right.0)
                    .then_with(|| left.1.cmp(&right.1))
            })
    }

    fn selection_bounds(&self) -> Option<(usize, usize)> {
        let selection = self.selection?;
        let start = selection.anchor.min(selection.head);
        let end = selection.anchor.max(selection.head);

        if start < self.glyphs.len() && end < self.glyphs.len() {
            Some((start, end))
        } else {
            None
        }
    }

    pub(crate) fn selected_text(&self) -> Option<String> {
        if self.view_language == PdfViewLanguage::Translated {
            let (start, _) = self.selection_bounds()?;
            let block_id = self.glyphs.get(start)?.block_idx;
            if let Some(block) = self.translated_blocks.get(&block_id) {
                return Some(block.text.clone());
            }
        }
        self.selected_source_text()
    }

    fn selected_source_text(&self) -> Option<String> {
        let (start, end) = self.selection_bounds()?;
        Self::text_for_glyph_range(&self.glyphs, start..end + 1)
    }

    fn selected_text_for_copy(&self, semantic: bool) -> Option<String> {
        if self.view_language == PdfViewLanguage::Translated || !semantic {
            return self.selected_text();
        }

        let blocks = self.selected_copy_blocks(true)?;
        Self::plain_text_for_blocks(&blocks)
    }

    fn selected_markdown(&self, semantic: bool) -> Option<String> {
        if self.view_language == PdfViewLanguage::Translated {
            return self
                .selected_text()
                .map(|text| Self::markdown_for_plain_text(&text));
        }

        let blocks = self.selected_copy_blocks(semantic)?;
        Some(Self::format_semantic_markdown(&blocks))
    }

    fn selected_copy_blocks(&self, semantic: bool) -> Option<Vec<TextFormatBlock>> {
        let (start, end) = self.selection_bounds()?;
        Self::copy_blocks_for_glyph_range(&self.glyphs, start..end + 1, semantic)
    }

    fn copy_blocks_for_glyph_range(
        glyphs: &[Glyph],
        range: Range<usize>,
        semantic: bool,
    ) -> Option<Vec<TextFormatBlock>> {
        let mut blocks = Self::text_blocks_for_glyph_range(glyphs, range)?;
        if semantic {
            for block in &mut blocks {
                block.lines = Self::semantic_logical_lines(&block.lines);
            }
            blocks = Self::merge_semantic_text_blocks(blocks);
        }
        Some(blocks)
    }

    fn merge_semantic_text_blocks(blocks: Vec<TextFormatBlock>) -> Vec<TextFormatBlock> {
        let mut merged = Vec::<TextFormatBlock>::with_capacity(blocks.len());

        for mut block in blocks {
            let can_merge = merged.last().is_some_and(|previous| {
                Self::text_blocks_are_continuous(previous, &block)
                    && block.lines.first().is_some_and(|line| {
                        let line = line.trim_start();
                        !line.is_empty()
                            && Self::strip_unordered_list_marker(line).is_none()
                            && Self::strip_ordered_list_marker(line).is_none()
                    })
            });

            if can_merge {
                let previous = merged.last_mut().unwrap();
                if let (Some(current), Some(continuation)) =
                    (previous.lines.last_mut(), block.lines.first())
                {
                    Self::append_semantic_continuation(current, continuation.trim_start());
                }
                previous.lines.extend(block.lines.drain(1..));
                previous.average_font_size =
                    (previous.average_font_size + block.average_font_size) / 2.0;
                previous.bold_ratio = (previous.bold_ratio + block.bold_ratio) / 2.0;
                if let (Some(previous_layout), Some(block_layout)) =
                    (&mut previous.layout, block.layout)
                {
                    previous_layout.last_line = block_layout.last_line;
                }
            } else {
                merged.push(block);
            }
        }

        merged
    }

    fn text_blocks_are_continuous(left: &TextFormatBlock, right: &TextFormatBlock) -> bool {
        let (Some(left_layout), Some(right_layout)) = (left.layout, right.layout) else {
            return false;
        };
        if left_layout.page_idx != right_layout.page_idx {
            return false;
        }

        let font_ratio = left.average_font_size.max(right.average_font_size)
            / left
                .average_font_size
                .min(right.average_font_size)
                .max(f32::EPSILON);
        if font_ratio > 1.15 {
            return false;
        }

        let left_rect = left_layout.last_line;
        let right_rect = right_layout.first_line;
        let line_height = (left_rect.y1 - left_rect.y0)
            .max(right_rect.y1 - right_rect.y0)
            .max(1.0);
        let vertical_gap = right_rect.y0 - left_rect.y1;
        if vertical_gap < -line_height * 0.25 || vertical_gap > line_height * 1.5 {
            return false;
        }

        let horizontal_overlap = left_rect.x1.min(right_rect.x1) - left_rect.x0.max(right_rect.x0);
        horizontal_overlap > 0.0
    }

    fn plain_text_for_blocks(blocks: &[TextFormatBlock]) -> Option<String> {
        let mut text = String::new();
        for (block_index, block) in blocks.iter().enumerate() {
            if block_index > 0 {
                text.push_str("\n\n");
            }
            for (line_index, line) in block.lines.iter().enumerate() {
                if line_index > 0 {
                    text.push('\n');
                }
                text.push_str(line);
            }
        }
        (!text.is_empty()).then_some(text)
    }

    fn semantic_logical_lines(lines: &[String]) -> Vec<String> {
        let mut logical_lines = Vec::<String>::new();

        for line in lines {
            let line = line.trim_end_matches(['\r', '\n']).trim_end();
            if line.is_empty() {
                if logical_lines.last().is_some_and(|line| !line.is_empty()) {
                    logical_lines.push(String::new());
                }
                continue;
            }

            let continuation = line.trim_start();
            let starts_list_item = Self::strip_unordered_list_marker(continuation).is_some()
                || Self::strip_ordered_list_marker(continuation).is_some();
            let should_join = logical_lines
                .last()
                .is_some_and(|line| !line.is_empty() && !starts_list_item);

            if should_join {
                let current = logical_lines.last_mut().unwrap();
                Self::append_semantic_continuation(current, continuation);
            } else {
                logical_lines.push(line.to_string());
            }
        }

        while logical_lines.last().is_some_and(String::is_empty) {
            logical_lines.pop();
        }
        logical_lines
    }

    fn append_semantic_continuation(current: &mut String, continuation: &str) {
        let dehyphenate = current.ends_with('-')
            && current.chars().rev().nth(1).is_some_and(Self::is_word_char)
            && continuation.chars().next().is_some_and(Self::is_word_char);
        if dehyphenate {
            current.pop();
        } else if !current.chars().last().is_some_and(char::is_whitespace) {
            current.push(' ');
        }
        current.push_str(continuation);
    }

    fn markdown_for_plain_text(text: &str) -> String {
        let blocks = text
            .split("\n\n")
            .map(|block| TextFormatBlock {
                lines: block.lines().map(str::to_string).collect(),
                average_font_size: 0.0,
                bold_ratio: 0.0,
                layout: None,
            })
            .filter(|block| !block.lines.is_empty())
            .collect::<Vec<_>>();
        Self::format_block_markdown(&blocks)
    }

    fn text_blocks_for_glyph_range(
        glyphs: &[Glyph],
        range: Range<usize>,
    ) -> Option<Vec<TextFormatBlock>> {
        if range.start >= range.end || range.end > glyphs.len() {
            return None;
        }

        let mut blocks = Vec::new();
        let mut current_block_idx = None;
        let mut current_line_idx = None;
        let mut lines = Vec::new();
        let mut line = String::new();
        let mut font_size_total = 0.0;
        let mut character_count = 0_usize;
        let mut bold_count = 0_usize;
        let mut previous_ix = None;
        let mut page_idx = None;
        let mut first_line_rect = None;
        let mut last_line_rect = None;

        let flush_line = |lines: &mut Vec<String>, line: &mut String| {
            lines.push(std::mem::take(line));
        };
        let flush_block = |blocks: &mut Vec<TextFormatBlock>,
                           lines: &mut Vec<String>,
                           font_size_total: &mut f32,
                           character_count: &mut usize,
                           bold_count: &mut usize,
                           page_idx: &mut Option<usize>,
                           first_line_rect: &mut Option<Rect>,
                           last_line_rect: &mut Option<Rect>| {
            if lines.iter().all(|line| line.trim().is_empty()) {
                lines.clear();
                *font_size_total = 0.0;
                *character_count = 0;
                *bold_count = 0;
                *page_idx = None;
                *first_line_rect = None;
                *last_line_rect = None;
                return;
            }
            blocks.push(TextFormatBlock {
                lines: std::mem::take(lines),
                average_font_size: if *character_count == 0 {
                    0.0
                } else {
                    *font_size_total / *character_count as f32
                },
                bold_ratio: if *character_count == 0 {
                    0.0
                } else {
                    *bold_count as f32 / *character_count as f32
                },
                layout: match (*page_idx, *first_line_rect, *last_line_rect) {
                    (Some(page_idx), Some(first_line), Some(last_line)) => Some(TextFormatLayout {
                        page_idx,
                        first_line,
                        last_line,
                    }),
                    _ => None,
                },
            });
            *font_size_total = 0.0;
            *character_count = 0;
            *bold_count = 0;
            *page_idx = None;
            *first_line_rect = None;
            *last_line_rect = None;
        };

        for glyph_ix in range {
            let glyph = &glyphs[glyph_ix];
            if current_block_idx != Some(glyph.block_idx) {
                if current_line_idx.is_some() {
                    flush_line(&mut lines, &mut line);
                }
                if current_block_idx.is_some() {
                    flush_block(
                        &mut blocks,
                        &mut lines,
                        &mut font_size_total,
                        &mut character_count,
                        &mut bold_count,
                        &mut page_idx,
                        &mut first_line_rect,
                        &mut last_line_rect,
                    );
                }
                current_block_idx = Some(glyph.block_idx);
                current_line_idx = None;
            } else if current_line_idx != Some(glyph.line_idx) && current_line_idx.is_some() {
                flush_line(&mut lines, &mut line);
            }

            page_idx.get_or_insert(glyph.page_idx);
            first_line_rect.get_or_insert(glyph.line_rect);
            last_line_rect = Some(glyph.line_rect);

            if current_line_idx == Some(glyph.line_idx)
                && let Some(previous_ix) = previous_ix
                && Self::has_inferred_space_between(glyphs, previous_ix, glyph_ix)
            {
                line.push(' ');
            }

            line.push(glyph.c);
            if !glyph.c.is_whitespace() {
                font_size_total += glyph.font_size.max(0.0);
                character_count += 1;
                bold_count += usize::from(glyph.is_bold);
            }
            current_line_idx = Some(glyph.line_idx);
            previous_ix = Some(glyph_ix);
        }

        if current_line_idx.is_some() {
            flush_line(&mut lines, &mut line);
        }
        if current_block_idx.is_some() {
            flush_block(
                &mut blocks,
                &mut lines,
                &mut font_size_total,
                &mut character_count,
                &mut bold_count,
                &mut page_idx,
                &mut first_line_rect,
                &mut last_line_rect,
            );
        }

        (!blocks.is_empty()).then_some(blocks)
    }

    pub(crate) fn document_text(&self) -> Option<String> {
        Self::text_for_glyph_range(&self.glyphs, 0..self.glyphs.len())
    }

    fn text_for_glyph_range(glyphs: &[Glyph], range: Range<usize>) -> Option<String> {
        if range.start >= range.end || range.end > glyphs.len() {
            return None;
        }

        let mut text = String::new();
        let mut previous_ix: Option<usize> = None;

        for glyph_ix in range {
            let glyph = &glyphs[glyph_ix];
            if let Some(previous_ix) = previous_ix {
                let previous = &glyphs[previous_ix];

                if glyph.block_idx != previous.block_idx {
                    text.push_str("\n\n");
                } else if glyph.line_idx != previous.line_idx {
                    text.push('\n');
                } else if Self::has_inferred_space_between(glyphs, previous_ix, glyph_ix) {
                    text.push(' ');
                }
            }

            text.push(glyph.c);
            previous_ix = Some(glyph_ix);
        }

        (!text.is_empty()).then_some(text)
    }

    fn copy_selection_as(&self, format: CopyFormat, cx: &mut Context<Self>) {
        let semantic = cx.global::<SettingsStore>().settings.behavior.semantic_copy;
        let text = match format {
            CopyFormat::PlainText => self.selected_text_for_copy(semantic),
            CopyFormat::Markdown => self.selected_markdown(semantic),
        };
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn copy_selection(&self, cx: &mut Context<Self>) {
        let format = cx.global::<SettingsStore>().settings.behavior.copy_format;
        self.copy_selection_as(format, cx);
    }

    fn can_translate_document(&self) -> bool {
        self.format == DocumentFormat::Pdf && self.path.is_absolute()
    }

    fn document_translation_output_path(source: &Path, target: &LanguageCode) -> PathBuf {
        let stem = source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("document");
        source
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(format!("{stem}.{}.pdf", target.as_str()))
    }

    fn translate_document(&mut self, cx: &mut Context<Self>) {
        if !self.can_translate_document() {
            return;
        }
        let source_path = self.path.clone();
        if self
            .document_translation_jobs
            .update(cx, |jobs, cx| jobs.reveal_for_source(&source_path, cx))
        {
            return;
        }

        let settings = cx.global::<SettingsStore>().settings.clone();
        let source_language = settings
            .translation
            .source_language
            .as_deref()
            .and_then(|language| LanguageCode::new(language).ok());
        let target_language = match LanguageCode::new(&settings.translation.target_language) {
            Ok(language) => language,
            Err(_) => {
                let source_path = self.path.clone();
                self.document_translation_jobs.update(cx, |jobs, cx| {
                    jobs.report_preflight_error(
                        source_path,
                        DocumentTranslationOpenMode::Document,
                        "Choose a valid target language in Translation Settings.",
                        cx,
                    );
                });
                return;
            }
        };
        let source_path = self.path.clone();
        let output_path = Self::document_translation_output_path(&source_path, &target_language);
        let directory = output_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let suggested_name = output_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("translated.pdf")
            .to_string();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |this, cx| {
            let result = dialog.await;
            _ = this.update(cx, |this, cx| match result {
                Ok(Ok(Some(output_path))) => {
                    let request = PdfTranslationRequest {
                        source_path,
                        output_path,
                        source_language,
                        target_language,
                    };
                    this.start_document_translation(
                        settings,
                        request,
                        DocumentTranslationOpenMode::Document,
                        cx,
                    );
                }
                Ok(Err(error)) => {
                    eprintln!("failed to choose translated PDF output path: {error:#}");
                }
                Ok(Ok(None)) | Err(_) => {}
            });
        })
        .detach();
    }

    fn start_document_translation(
        &mut self,
        settings: crate::settings::Settings,
        request: PdfTranslationRequest,
        open_mode: DocumentTranslationOpenMode,
        cx: &mut Context<Self>,
    ) {
        if !self.can_translate_document() || request.source_path != self.path {
            return;
        }
        let source_path = self.path.clone();
        if self
            .document_translation_jobs
            .update(cx, |jobs, cx| jobs.reveal_for_source(&source_path, cx))
        {
            return;
        }

        if normalize_path(&request.output_path) == self.path {
            let source_path = self.path.clone();
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.report_preflight_error(
                    source_path,
                    open_mode,
                    "Choose an output path different from the source PDF.",
                    cx,
                );
            });
            return;
        }

        if !cx.has_global::<PdfTranslationService>() {
            let source_path = self.path.clone();
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.report_preflight_error(
                    source_path,
                    open_mode,
                    "Document translation is unavailable.",
                    cx,
                );
            });
            return;
        }

        self.document_translation_jobs.update(cx, |jobs, cx| {
            jobs.submit(settings, request, open_mode, cx);
        });
    }

    fn cancel_document_translation(&mut self, cx: &mut Context<Self>) {
        let source_path = self.path.clone();
        self.document_translation_jobs.update(cx, |jobs, cx| {
            jobs.cancel_for_source(&source_path, cx);
        });
    }

    fn open_translation_split(&mut self, cx: &mut Context<Self>) {
        if !self.can_translate_document() {
            return;
        }
        let source_path = self.path.clone();
        if self
            .document_translation_jobs
            .update(cx, |jobs, cx| jobs.reveal_for_source(&source_path, cx))
        {
            return;
        }
        if !cx.has_global::<PdfTranslationService>() {
            let source_path = self.path.clone();
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.report_preflight_error(
                    source_path,
                    DocumentTranslationOpenMode::Split,
                    "Document translation is unavailable.",
                    cx,
                );
            });
            return;
        }

        let settings = cx.global::<SettingsStore>().settings.clone();
        let source_language = settings
            .translation
            .source_language
            .as_deref()
            .and_then(|language| LanguageCode::new(language).ok());
        let target_language = match LanguageCode::new(&settings.translation.target_language) {
            Ok(language) => language,
            Err(_) => {
                let source_path = self.path.clone();
                self.document_translation_jobs.update(cx, |jobs, cx| {
                    jobs.report_preflight_error(
                        source_path,
                        DocumentTranslationOpenMode::Split,
                        "Choose a valid target language in Translation Settings.",
                        cx,
                    );
                });
                return;
            }
        };
        let service = cx.global::<PdfTranslationService>().clone();
        let request = match service.artifact_request(
            &settings,
            self.path.clone(),
            source_language,
            target_language,
        ) {
            Ok(request) => request,
            Err(error) => {
                let source_path = self.path.clone();
                let message = error.message;
                self.document_translation_jobs.update(cx, |jobs, cx| {
                    jobs.report_preflight_error(
                        source_path,
                        DocumentTranslationOpenMode::Split,
                        message,
                        cx,
                    );
                });
                return;
            }
        };
        match service.cached_artifact(&settings, &request) {
            Some(result) => {
                self.document_translation_jobs.update(cx, |jobs, cx| {
                    jobs.open_cached_output(
                        settings,
                        request,
                        DocumentTranslationOpenMode::Split,
                        result.output_path,
                        cx,
                    );
                });
            }
            None => self.start_document_translation(
                settings,
                request,
                DocumentTranslationOpenMode::Split,
                cx,
            ),
        }
    }

    fn translate_selection(
        &mut self,
        popup_position: Option<GpuiPoint<Pixels>>,
        update_pinned: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(text) = self.selected_source_text() else {
            return;
        };
        self.translate_text(text, popup_position, update_pinned, cx);
    }

    fn reset_translation_scroll(&self, pinned: bool) {
        let scroll_handle = if pinned {
            &self.pinned_translation_scroll
        } else {
            &self.translation_popup_scroll
        };
        scroll_handle.set_offset(point(px(0.0), px(0.0)));
    }

    fn translate_text(
        &mut self,
        text: String,
        popup_position: Option<GpuiPoint<Pixels>>,
        update_pinned: bool,
        cx: &mut Context<Self>,
    ) {
        let settings = cx.global::<SettingsStore>().settings.clone();
        let target_language = match LanguageCode::new(&self.target_language) {
            Ok(language) => language,
            Err(error) => {
                self.set_translation_error(
                    popup_position,
                    update_pinned,
                    text,
                    error.to_string(),
                    cx,
                );
                return;
            }
        };
        self.translation_request_generation = self.translation_request_generation.wrapping_add(1);
        let generation = self.translation_request_generation;
        let state = TranslationCardState::new(
            text.clone(),
            TranslationStatus::Loading,
            popup_position,
            generation,
        );
        if popup_position.is_some() {
            self.reset_translation_scroll(false);
            self.translation_popup = Some(state);
        }
        if update_pinned {
            self.reset_translation_scroll(true);
            self.pinned_translation = Some(TranslationCardState::new(
                text.clone(),
                TranslationStatus::Loading,
                None,
                generation,
            ));
        }
        cx.notify();

        if !cx.has_global::<TranslationService>() {
            self.finish_translation_request(
                generation,
                Err("translation service is unavailable".to_string()),
                cx,
            );
            return;
        }

        let service = cx.global::<TranslationService>().clone();
        let provider = settings.translation.active_provider;
        let source_language = settings
            .translation
            .source_language
            .as_deref()
            .and_then(|language| LanguageCode::new(language).ok());
        let request = TranslationRequest {
            provider,
            source_language,
            target_language,
            texts: vec![text],
            context_before: None,
            context_after: None,
        };

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    service
                        .translate(&settings, request)
                        .map(|response| {
                            response.translations.into_iter().next().unwrap_or_default()
                        })
                        .map_err(|error| error.to_string())
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.finish_translation_request(generation, result, cx);
            });
        })
        .detach();
    }

    fn request_document_translation(&mut self, cx: &mut Context<Self>) {
        if self.view_language != PdfViewLanguage::Translated
            || self.text_blocks.is_empty()
            || !cx.has_global::<TranslationService>()
        {
            return;
        }
        self.translation_document_generation = self.translation_document_generation.wrapping_add(1);
        let generation = self.translation_document_generation;
        let blocks = self
            .text_blocks
            .iter()
            .filter(|block| block.horizontal && !block.text.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>();
        let settings = cx.global::<SettingsStore>().settings.clone();
        let provider = settings.translation.active_provider;
        let source_language = settings
            .translation
            .source_language
            .as_deref()
            .and_then(|language| LanguageCode::new(language).ok());
        let target_language = match LanguageCode::new(&self.target_language) {
            Ok(language) => language,
            Err(error) => {
                eprintln!("invalid translation target language: {error:#}");
                return;
            }
        };
        let service = cx.global::<TranslationService>().clone();

        self.translation_document_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let texts = blocks
                        .iter()
                        .map(|block| block.text.clone())
                        .collect::<Vec<_>>();
                    service
                        .translate_batches(
                            &settings,
                            provider,
                            source_language,
                            target_language,
                            texts,
                        )
                        .map(|translations| (blocks, translations))
                        .map_err(|error| error.to_string())
                })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.translation_document_generation != generation {
                    return;
                }
                this.translation_document_task = None;
                match result {
                    Ok((blocks, translations)) if blocks.len() == translations.len() => {
                        this.translated_blocks = blocks
                            .into_iter()
                            .zip(translations)
                            .map(|(source, text)| (source.id, TranslatedBlock { source, text }))
                            .collect();
                        this.translated_text_layouts.clear();
                    }
                    Ok(_) => {
                        eprintln!("translation provider returned an incomplete document result");
                    }
                    Err(error) => eprintln!("failed to translate document: {error}"),
                }
                cx.notify();
            });
        }));
    }

    fn set_translation_error(
        &mut self,
        popup_position: Option<GpuiPoint<Pixels>>,
        update_pinned: bool,
        source: String,
        error: String,
        cx: &mut Context<Self>,
    ) {
        self.translation_request_generation = self.translation_request_generation.wrapping_add(1);
        let error: SharedString = error.into();
        let state = TranslationCardState::new(
            source.clone(),
            TranslationStatus::Error(error.clone()),
            popup_position,
            self.translation_request_generation,
        );
        if popup_position.is_some() {
            self.reset_translation_scroll(false);
            self.translation_popup = Some(state);
        }
        if update_pinned {
            self.reset_translation_scroll(true);
            self.pinned_translation = Some(TranslationCardState::new(
                source,
                TranslationStatus::Error(error),
                None,
                self.translation_request_generation,
            ));
        }
        cx.notify();
    }

    fn finish_translation_request(
        &mut self,
        generation: u64,
        result: std::result::Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let status = match result {
            Ok(text) if !text.trim().is_empty() => TranslationStatus::Ready {
                markdown: cx.new(|cx| Markdown::new(text.clone().into(), cx)),
                text: text.into(),
            },
            Ok(_) => TranslationStatus::Error(crate::fl!("translation-no-result").into()),
            Err(error) => TranslationStatus::Error(error.into()),
        };
        if let Some(popup) = self.translation_popup.as_mut()
            && popup.request_generation == generation
        {
            popup.status = status.clone();
        }
        if let Some(pinned) = self.pinned_translation.as_mut()
            && pinned.request_generation == generation
        {
            pinned.status = status;
        }
        cx.notify();
    }

    fn toggle_pinned_translation(&mut self, cx: &mut Context<Self>) {
        if self.pinned_translation.is_none() {
            self.reset_translation_scroll(true);
            self.pinned_translation = Some(TranslationCardState::new(
                SharedString::default(),
                TranslationStatus::Idle,
                None,
                self.translation_request_generation,
            ));
        } else {
            self.pinned_translation = None;
        }
        cx.notify();
    }

    fn change_target_language(&mut self, target_language: String, cx: &mut Context<Self>) {
        let Ok(target_language) = LanguageCode::new(target_language) else {
            return;
        };
        if self.target_language == target_language.as_str() {
            return;
        }
        self.target_language = target_language.as_str().to_string();
        for dropdown in [
            self.translation_popup_language_dropdown.as_ref(),
            self.pinned_translation_language_dropdown.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            dropdown.update(cx, |dropdown, cx| {
                dropdown.set_selected_value(target_language.as_str().to_string());
                cx.notify();
            });
        }
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_document_target_language(&self.path, target_language.as_str())
        {
            eprintln!("failed to save document translation language: {error:#}");
        }

        let popup = self
            .translation_popup
            .as_ref()
            .map(|state| (state.source.to_string(), state.position));
        let pinned_source = self
            .pinned_translation
            .as_ref()
            .map(|state| state.source.to_string());

        match (popup, pinned_source) {
            (Some((source, position)), Some(pinned_source))
                if !source.is_empty() && source == pinned_source =>
            {
                self.translate_text(source, position, true, cx);
            }
            (popup, pinned_source) => {
                if let Some((source, position)) = popup {
                    self.translate_text(source, position, false, cx);
                }
                if let Some(source) = pinned_source
                    && !source.is_empty()
                {
                    self.translate_text(source, None, true, cx);
                }
            }
        }
        if self.view_language == PdfViewLanguage::Translated {
            self.request_document_translation(cx);
        }
        cx.notify();
    }

    fn close_translation_popup(&mut self, cx: &mut Context<Self>) {
        if self.translation_popup.take().is_some() {
            cx.notify();
        }
    }

    fn render_translation_card(
        &self,
        state: TranslationCardState,
        pinned: bool,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let language_dropdown = if pinned {
            self.pinned_translation_language_dropdown.clone()
        } else {
            self.translation_popup_language_dropdown.clone()
        };
        let language_dropdown_open = language_dropdown
            .as_ref()
            .is_some_and(|dropdown| dropdown.read(cx).is_open());
        let font = &cx.global::<SettingsStore>().settings.font;
        let base_text_style = TextStyle {
            font_family: font.ui_font_family.clone().into(),
            font_size: ui_rems(13.0).into(),
            color: rgb(theme.text).into(),
            font_weight: FontWeight(font.ui_font_weight),
            ..Default::default()
        };
        let content = match &state.status {
            TranslationStatus::Idle => div().into_any_element(),
            TranslationStatus::Loading => div()
                .py_2()
                .text_color(rgb(theme.text_muted))
                .child(crate::fl!("translation-loading"))
                .into_any_element(),
            TranslationStatus::Ready { markdown, .. } => div()
                .w_full()
                .min_w_0()
                .child(MarkdownElement::new(
                    markdown.clone(),
                    MarkdownStyle::from_theme(theme, base_text_style),
                ))
                .into_any_element(),
            TranslationStatus::Error(error) => div()
                .py_2()
                .text_color(rgb(theme.error))
                .child(error.clone())
                .into_any_element(),
        };
        let copy_text = match &state.status {
            TranslationStatus::Ready { text, .. } => Some(text.clone()),
            _ => None,
        };
        let scrollable = matches!(
            state.status,
            TranslationStatus::Ready { .. } | TranslationStatus::Error(_)
        );
        let scroll_handle = if pinned {
            &self.pinned_translation_scroll
        } else {
            &self.translation_popup_scroll
        };
        let scrollbar_id = if pinned {
            "pinned-translation-scrollbar"
        } else {
            "translation-popup-scrollbar"
        };

        div()
            .occlude()
            .w(px(cx
                .global::<SettingsStore>()
                .settings
                .translation
                .pinned
                .width))
            .overflow_hidden()
            .flex()
            .flex_col()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .text_size(ui_rems(13.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .when(!pinned && !language_dropdown_open, |this| {
                this.on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_translation_popup(cx);
                }))
            })
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .p_2()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.border_variant))
                    .text_size(ui_rems(11.0))
                    .text_color(rgb(theme.text_muted))
                    .when_some(language_dropdown, |this, dropdown| this.child(dropdown))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when_some(copy_text, |this, text| {
                                this.child(
                                    IconButton::new(
                                        "translation-popup-copy",
                                        IconName::Copy,
                                        rgb(theme.text_muted),
                                        rgb(theme.element_hover),
                                    )
                                    .into_div()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |_, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.to_string(),
                                            ));
                                            cx.stop_propagation();
                                        }),
                                    ),
                                )
                            })
                            .child(
                                IconButton::new(
                                    "translation-popup-dismiss",
                                    IconName::X,
                                    rgb(theme.text_muted),
                                    rgb(theme.element_hover),
                                )
                                .into_div()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        if pinned {
                                            this.toggle_pinned_translation(cx);
                                        } else {
                                            this.close_translation_popup(cx);
                                        }
                                        cx.stop_propagation();
                                    }),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .id(if pinned {
                                "pinned-translation-content"
                            } else {
                                "translation-popup-content"
                            })
                            .track_scroll(scroll_handle)
                            .w_full()
                            .min_w_0()
                            .max_h(px(300.0))
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .p_3()
                            .cursor_text()
                            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                            .child(content),
                    )
                    .when(scrollable, |this| {
                        this.vertical_scrollbar_for_with_id(scroll_handle, scrollbar_id, window, cx)
                    }),
            )
            .into_any_element()
    }

    fn select_all(&mut self) {
        self.selected_highlight = None;
        if self.glyphs.is_empty() {
            self.selection = None;
            return;
        }

        self.selection = Some(Selection {
            anchor: 0,
            head: self.glyphs.len() - 1,
        });
    }

    fn create_highlight_from_selection(&mut self, color: HighlightColor, cx: &mut Context<Self>) {
        let Some((start, end)) = self.selection_bounds() else {
            return;
        };
        let Some(text) = self.selected_text() else {
            return;
        };
        let rects = self.highlight_rects_for_range(start..end + 1);
        if rects.is_empty() {
            return;
        }

        let path = self.path.clone();
        let result = if self.capabilities.reflow {
            let semantic_range = self.semantic_range_for_selection(start, end);
            cx.update_global::<LibraryStore, _>(move |store, _| {
                store.add_reflow_highlight(&path, color, semantic_range, rects, text, "")
            })
        } else {
            cx.update_global::<LibraryStore, _>(move |store, _| {
                store.add_highlight(&path, color, rects, text, "")
            })
        };
        match result {
            Ok(highlight) => {
                self.selected_highlight = Some(highlight.id);
                self.highlights.push(highlight);
                self.selection = None;
                cx.notify();
            }
            Err(error) => eprintln!("failed to add PDF highlight: {error:#}"),
        }
    }

    fn set_context_highlight_color(&mut self, color: HighlightColor, cx: &mut Context<Self>) {
        let Some(id) = self.context_menu_highlight.or(self.selected_highlight) else {
            return;
        };

        match cx.update_global::<LibraryStore, _>(|store, _| store.set_highlight_color(id, color)) {
            Ok(true) => {
                if let Some(highlight) = self
                    .highlights
                    .iter_mut()
                    .find(|highlight| highlight.id == id)
                {
                    highlight.color = color;
                }
                self.selected_highlight = Some(id);
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => eprintln!("failed to update PDF highlight: {error:#}"),
        }
    }

    fn remove_context_highlight(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.context_menu_highlight.or(self.selected_highlight) else {
            return;
        };

        match cx.update_global::<LibraryStore, _>(|store, _| store.remove_highlight(id)) {
            Ok(true) => {
                self.highlights.retain(|highlight| highlight.id != id);
                if self.selected_highlight == Some(id) {
                    self.selected_highlight = None;
                }
                self.context_menu_highlight = None;
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => eprintln!("failed to remove PDF highlight: {error:#}"),
        }
    }

    fn begin_add_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((start, end)) = self.selection_bounds() else {
            return;
        };
        let Some(text) = self.selected_text() else {
            return;
        };
        let rects = self.highlight_rects_for_range(start..end + 1);
        if rects.is_empty() {
            return;
        }

        self.open_note_editor(
            "".into(),
            HighlightColor::Yellow,
            NoteTarget::New {
                rects,
                semantic_range: self
                    .capabilities
                    .reflow
                    .then(|| self.semantic_range_for_selection(start, end)),
                text,
            },
            window,
            cx,
        );
    }

    fn begin_edit_context_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.context_menu_note.or(self.hovered_note) else {
            return;
        };
        let Some((body, color)) = self
            .notes
            .iter()
            .find(|note| note.id == id)
            .map(|note| (note.body.clone(), note.color))
        else {
            return;
        };

        self.open_note_editor(body.into(), color, NoteTarget::Edit { id }, window, cx);
    }

    fn open_note_editor(
        &mut self,
        content: SharedString,
        color: HighlightColor,
        target: NoteTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let editor =
            cx.new(|cx| NoteEditor::new(previous_focus_handle.clone(), content, color, cx));

        cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
            NoteEditorEvent::Save { body, color } => {
                this.save_note_editor(body.clone(), *color, window, cx);
            }
            NoteEditorEvent::Cancel => {
                this.close_note_editor(window, cx);
            }
        })
        .detach();

        editor.focus_handle(cx).focus(window, cx);
        self.note_editor = Some(editor);
        self.note_target = Some(target);
        cx.notify();
    }

    fn save_note_editor(
        &mut self,
        body: SharedString,
        color: HighlightColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let body = body.as_ref().trim().to_string();
        if body.is_empty() {
            self.close_note_editor(window, cx);
            return;
        }

        let Some(target) = self.note_target.take() else {
            self.close_note_editor(window, cx);
            return;
        };

        match target {
            NoteTarget::New {
                rects,
                semantic_range,
                text,
            } => {
                let path = self.path.clone();
                let result = if let Some(semantic_range) = semantic_range {
                    cx.update_global::<LibraryStore, _>(move |store, _| {
                        store.add_reflow_note(&path, color, semantic_range, rects, text, body)
                    })
                } else {
                    cx.update_global::<LibraryStore, _>(move |store, _| {
                        store.add_note(&path, color, rects, text, body)
                    })
                };
                match result {
                    Ok(note) => {
                        let note_id = note.id;
                        self.notes.push(note);
                        self.notes_sidebar_selected = Some(note_id);
                        self.refresh_notes_sidebar_entries();
                        self.selection = None;
                    }
                    Err(error) => eprintln!("failed to add PDF note: {error:#}"),
                }
            }
            NoteTarget::Edit { id } => {
                match cx.update_global::<LibraryStore, _>(|store, _| {
                    store.update_note(id, body.clone(), color)
                }) {
                    Ok(true) => {
                        if let Some(note) = self.notes.iter_mut().find(|note| note.id == id) {
                            note.body = body;
                            note.color = color;
                            self.notes_sidebar_selected = Some(id);
                            self.refresh_notes_sidebar_entries();
                        }
                    }
                    Ok(false) => {}
                    Err(error) => eprintln!("failed to update PDF note: {error:#}"),
                }
            }
        }

        self.note_editor = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn close_note_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.note_editor.take().is_some() {
            self.note_target = None;
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn remove_context_note(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.context_menu_note.or(self.hovered_note) else {
            return;
        };

        match cx.update_global::<LibraryStore, _>(|store, _| store.remove_note(id)) {
            Ok(true) => {
                self.notes.retain(|note| note.id != id);
                if self.notes_sidebar_selected == Some(id) {
                    self.notes_sidebar_selected = None;
                }
                self.refresh_notes_sidebar_entries();
                if self.hovered_note == Some(id) {
                    self.hovered_note = None;
                    self.hovered_note_position = None;
                }
                self.context_menu_note = None;
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => eprintln!("failed to remove PDF note: {error:#}"),
        }
    }

    fn deploy_search_query(&mut self, query: SharedString, previous: bool, cx: &mut Context<Self>) {
        if query.as_ref() == self.search.query.as_ref() {
            self.search
                .update_reveal_after_indexing(self.text_index_state.is_pending_or_indexing());
            self.navigate_search(previous, cx);
            return;
        }

        self.search.query = query;
        self.search
            .update_reveal_after_indexing(self.text_index_state.is_pending_or_indexing());
        self.recompute_search();
        self.search.active = self.nearest_search_match_index();
        if previous
            && let Some(active) = self.search.active
            && !self.search.matches.is_empty()
        {
            self.search.active = Some(
                active
                    .checked_sub(1)
                    .unwrap_or(self.search.matches.len() - 1),
            );
        }
        self.sync_search_bar(cx);

        if self.search.active.is_some() {
            self.record_search_jump_origin();
            self.scroll_to_active_search_match(cx);
        }

        cx.notify();
    }

    fn live_search(&mut self, query: SharedString, cx: &mut Context<Self>) {
        if query.as_ref() == self.search.query.as_ref() {
            return;
        }

        self.search.query = query;
        self.search
            .update_reveal_after_indexing(self.text_index_state.is_pending_or_indexing());
        self.recompute_search();
        self.search.active = self.nearest_search_match_index();
        self.sync_search_bar(cx);

        if self.search.active.is_some() {
            self.record_search_jump_origin();
            self.scroll_to_active_search_match(cx);
        }

        cx.notify();
    }

    fn set_search_ignore_case(
        &mut self,
        query: SharedString,
        ignore_case: bool,
        cx: &mut Context<Self>,
    ) {
        if query.as_ref() == self.search.query.as_ref() && ignore_case == self.search.ignore_case {
            return;
        }

        self.search.query = query;
        self.search.ignore_case = ignore_case;
        self.search
            .update_reveal_after_indexing(self.text_index_state.is_pending_or_indexing());
        self.recompute_search();
        self.search.active = self.nearest_search_match_index();
        self.sync_search_bar(cx);

        if self.search.active.is_some() {
            self.record_search_jump_origin();
            self.scroll_to_active_search_match(cx);
        }

        cx.notify();
    }

    fn recompute_search(&mut self) {
        self.search.matches.clear();
        self.search.active = None;

        let ignore_case = self.search.ignore_case;
        let needle = if ignore_case {
            self.search.query.as_ref().to_lowercase()
        } else {
            self.search.query.as_ref().to_string()
        };
        if needle.is_empty() || self.text_index_state.is_pending_or_indexing() {
            return;
        }

        for page_idx in 0..self.pages.len() {
            let (haystack, glyph_map) = self.searchable_page_text(page_idx, ignore_case);
            let mut start_byte = 0;

            while let Some(relative_start) = haystack[start_byte..].find(&needle) {
                let match_start = start_byte + relative_start;
                let match_end = match_start + needle.len();
                let start_char = Self::char_index_for_byte(&haystack, match_start);
                let end_char = Self::char_index_for_byte(&haystack, match_end);

                if let Some(range) =
                    Self::glyph_range_for_search_chars(&glyph_map, start_char, end_char)
                {
                    self.search.matches.push(SearchMatch { page_idx, range });
                }

                start_byte = match_end;
            }
        }
    }

    fn searchable_page_text(
        &self,
        page_idx: usize,
        ignore_case: bool,
    ) -> (String, Vec<Option<usize>>) {
        let mut text = String::new();
        let mut glyph_map = Vec::new();
        let mut previous: Option<&Glyph> = None;
        let Some(page) = self.pages.get(page_idx) else {
            return (text, glyph_map);
        };
        let range = page.glyph_range.clone();
        if range.start >= range.end || range.end > self.glyphs.len() {
            return (text, glyph_map);
        }

        for glyph_ix in range {
            let glyph = &self.glyphs[glyph_ix];
            if let Some(previous) = previous {
                if glyph.block_idx != previous.block_idx {
                    Self::push_search_separator(&mut text, &mut glyph_map);
                    Self::push_search_separator(&mut text, &mut glyph_map);
                } else if glyph.line_idx != previous.line_idx || previous.inferred_space_after {
                    Self::push_search_separator(&mut text, &mut glyph_map);
                }
            }

            if ignore_case {
                for ch in glyph.c.to_lowercase() {
                    text.push(ch);
                    glyph_map.push(Some(glyph_ix));
                }
            } else {
                text.push(glyph.c);
                glyph_map.push(Some(glyph_ix));
            }
            previous = Some(glyph);
        }

        (text, glyph_map)
    }

    fn push_search_separator(text: &mut String, glyph_map: &mut Vec<Option<usize>>) {
        text.push('\n');
        glyph_map.push(None);
    }

    fn char_index_for_byte(text: &str, byte_ix: usize) -> usize {
        text[..byte_ix].chars().count()
    }

    fn glyph_range_for_search_chars(
        glyph_map: &[Option<usize>],
        start_char: usize,
        end_char: usize,
    ) -> Option<Range<usize>> {
        if start_char >= end_char || end_char > glyph_map.len() {
            return None;
        }

        let start = glyph_map[start_char..end_char]
            .iter()
            .find_map(|glyph_ix| *glyph_ix)?;
        let end = glyph_map[start_char..end_char]
            .iter()
            .rev()
            .find_map(|glyph_ix| *glyph_ix)?
            + 1;

        (start < end).then_some(start..end)
    }

    fn nearest_search_match_index(&self) -> Option<usize> {
        if self.search.matches.is_empty() {
            return None;
        }

        self.search
            .matches
            .iter()
            .enumerate()
            .find_map(|(ix, search_match)| {
                self.search_match_doc_rect(search_match)
                    .and_then(|rect| (rect.y1 >= self.scroll_y_doc).then_some(ix))
            })
            .or(Some(0))
    }

    fn navigate_search(&mut self, previous: bool, cx: &mut Context<Self>) {
        let match_count = self.search.matches.len();
        if match_count == 0 {
            self.sync_search_bar(cx);
            cx.notify();
            return;
        }

        let next = match self.search.active {
            Some(active) if previous => active.checked_sub(1).unwrap_or(match_count - 1),
            Some(active) => (active + 1) % match_count,
            None if previous => match_count - 1,
            None => 0,
        };

        self.search.active = Some(next);
        self.sync_search_bar(cx);
        self.record_search_jump_origin();
        self.scroll_to_search_match(next, cx);
        cx.notify();
    }

    fn scroll_to_active_search_match(&mut self, cx: &mut Context<Self>) {
        if let Some(active) = self.search.active {
            self.scroll_to_search_match(active, cx);
        }
    }

    fn scroll_to_search_match(&mut self, match_ix: usize, cx: &mut Context<Self>) {
        let Some(search_match) = self.search.matches.get(match_ix) else {
            return;
        };
        let Some(doc_rect) = self.search_match_doc_rect(search_match) else {
            return;
        };

        let page_width = self.pages[search_match.page_idx].width;
        let scale = self.effective_scale();
        let center_x = (doc_rect.x0 + doc_rect.x1) / 2.0;
        let center_y = (doc_rect.y0 + doc_rect.y1) / 2.0;

        self.set_scroll_doc(
            center_x - page_width / 2.0,
            center_y - self.viewport_height / (2.0 * scale),
            cx,
        );
    }

    fn search_match_doc_rect(&self, search_match: &SearchMatch) -> Option<Rect> {
        if search_match.range.start >= search_match.range.end
            || search_match.range.end > self.glyphs.len()
            || search_match.page_idx >= self.pages.len()
        {
            return None;
        }

        let mut rect = None;

        for glyph in &self.glyphs[search_match.range.clone()] {
            if glyph.page_idx != search_match.page_idx {
                continue;
            }

            let glyph_rect = Self::quad_rect(&glyph.quad);
            rect = Some(match rect {
                Some(existing) => Self::union_rect(existing, glyph_rect),
                None => glyph_rect,
            });
        }

        let rect = rect?;
        let page = &self.pages[search_match.page_idx];
        Some(Rect::new(
            rect.x0 - page.bounds.x0,
            page.offset_y + rect.y0 - page.bounds.y0,
            rect.x1 - page.bounds.x0,
            page.offset_y + rect.y1 - page.bounds.y0,
        ))
    }

    fn sync_search_bar(&mut self, cx: &mut Context<Self>) {
        if let Some(search_bar) = self.search_bar.clone() {
            let query = self.search.query.clone();
            let active = self.search.active;
            let match_count = self.search.matches.len();
            let ignore_case = self.search.ignore_case;
            let indexing = self.text_index_state.is_pending_or_indexing() && !query.is_empty();
            search_bar.update(cx, |search_bar, cx| {
                search_bar.set_match_status(
                    query.clone(),
                    active,
                    match_count,
                    ignore_case,
                    indexing,
                    cx,
                );
            });
        }
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.zoom).abs() <= f32::EPSILON {
            return;
        }

        let old_scale = self.effective_scale();
        let center_doc = self.scroll_y_doc + self.viewport_height / (2.0 * old_scale);

        self.zoom = zoom;

        let new_scale = self.effective_scale();
        self.scroll_y_doc = center_doc - self.viewport_height / (2.0 * new_scale);
        self.target_scroll_x_doc = self.scroll_x_doc;
        self.target_scroll_y_doc = self.scroll_y_doc;
        self.clamp_scroll();
        self.hovered_link = None;
        self.pending_link = None;
        self.schedule_position_save(cx);
        self.emit_linked_viewport(cx);
        cx.notify();
    }

    fn zoom_in(&mut self, cx: &mut Context<Self>) {
        let zoom_step = cx.global::<SettingsStore>().settings.behavior.zoom_step;
        if self.grid_overview_open {
            self.set_grid_overview_zoom(self.grid_overview_zoom * zoom_step, cx);
            return;
        }
        if self.is_presentation_mode() {
            return;
        }

        self.set_zoom(self.zoom * zoom_step, cx);
    }

    fn zoom_out(&mut self, cx: &mut Context<Self>) {
        let zoom_step = cx.global::<SettingsStore>().settings.behavior.zoom_step;
        if self.grid_overview_open {
            self.set_grid_overview_zoom(self.grid_overview_zoom / zoom_step, cx);
            return;
        }
        if self.is_presentation_mode() {
            return;
        }

        self.set_zoom(self.zoom / zoom_step, cx);
    }

    fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        if self.is_presentation_mode() {
            return;
        }

        let default_zoom = cx.global::<SettingsStore>().settings.behavior.default_zoom;
        self.set_zoom(default_zoom, cx);
    }

    fn set_grid_overview_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(GRID_OVERVIEW_MIN_ZOOM, GRID_OVERVIEW_MAX_ZOOM);
        if (zoom - self.grid_overview_zoom).abs() <= f32::EPSILON {
            return;
        }

        self.grid_overview_zoom = zoom;
        self.update_grid_overview_cols(self.grid_overview_viewport_width, cx);
        cx.notify();
    }

    fn page_index_for_scroll_y(&self, scroll_y_doc: f32) -> usize {
        let scale = self.effective_scale();
        let center_doc = scroll_y_doc + self.viewport_height / (2.0 * scale);
        let mut current = 0;

        for (idx, page) in self.pages.iter().enumerate() {
            if center_doc >= page.offset_y {
                current = idx;
            } else {
                break;
            }
        }

        current
    }

    fn page_index_for_doc_y(&self, doc_y: f32) -> usize {
        let mut current = 0;
        for (idx, page) in self.pages.iter().enumerate() {
            if doc_y >= page.offset_y {
                current = idx;
            } else {
                break;
            }
        }

        current
    }

    pub fn is_presentation_mode(&self) -> bool {
        self.presentation_page.is_some()
    }

    fn presentation_page_index(&self) -> Option<usize> {
        self.presentation_page
            .map(|page_idx| page_idx.min(self.last_page_index()))
    }

    fn page_scroll_y_doc(&self, page_idx: usize) -> f32 {
        self.pages
            .get(page_idx.min(self.last_page_index()))
            .map(|page| page.offset_y)
            .unwrap_or(0.0)
    }

    fn current_page_index(&self) -> usize {
        if let Some(page_idx) = self.presentation_page_index() {
            return page_idx;
        }

        self.page_index_for_scroll_y(self.scroll_y_doc)
    }

    pub(crate) fn current_page(&self) -> usize {
        self.current_page_index() + 1
    }

    pub(crate) fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page_counter_label(current_page: usize, page_count: usize) -> String {
        format!("{current_page} / {page_count}")
    }

    fn last_page_index(&self) -> usize {
        self.pages.len().saturating_sub(1)
    }

    fn set_presentation_page(&mut self, page_idx: usize, cx: &mut Context<Self>) -> bool {
        let page_idx = page_idx.min(self.last_page_index());
        if self.presentation_page == Some(page_idx) {
            return false;
        }

        self.presentation_page = Some(page_idx);
        self.hovered_link = None;
        self.hovered_note = None;
        self.hovered_note_position = None;
        self.pending_link = None;
        self.schedule_position_save(cx);
        cx.notify();
        true
    }

    fn navigate_presentation_by_direction(&mut self, direction: f32, cx: &mut Context<Self>) {
        if direction > 0.0 {
            if self.set_presentation_page(self.current_page_index() + 1, cx) {
                self.mark_manual_navigation();
            }
        } else if direction < 0.0
            && self.set_presentation_page(self.current_page_index().saturating_sub(1), cx)
        {
            self.mark_manual_navigation();
        }
    }

    fn navigate_to_page_start(&mut self, page_idx: usize, cx: &mut Context<Self>) {
        let target_page = page_idx.min(self.last_page_index());
        if self.is_presentation_mode() {
            if self.set_presentation_page(target_page, cx) {
                self.mark_manual_navigation();
            }
            return;
        }

        let target = self
            .pages
            .get(target_page)
            .map(|page| page.offset_y)
            .unwrap_or_else(|| self.max_scroll_y_doc());

        if self.set_scroll_doc_target(self.target_scroll_x_doc, target, cx) {
            self.mark_manual_navigation();
        }
    }

    fn grid_overview_cursor_row(&self) -> usize {
        self.grid_overview_cursor / self.grid_overview_cols.max(1)
    }

    fn scroll_grid_overview_cursor(&self, strategy: ScrollStrategy) {
        self.grid_overview_scroll
            .scroll_to_item(self.grid_overview_cursor_row(), strategy);
    }

    fn set_grid_overview_cursor(
        &mut self,
        page_idx: usize,
        strategy: ScrollStrategy,
        cx: &mut Context<Self>,
    ) {
        self.grid_overview_cursor = page_idx.min(self.last_page_index());
        self.scroll_grid_overview_cursor(strategy);
        cx.notify();
    }

    fn move_grid_overview_cursor(
        &mut self,
        delta: isize,
        strategy: ScrollStrategy,
        cx: &mut Context<Self>,
    ) {
        let target = if delta < 0 {
            self.grid_overview_cursor
                .saturating_sub(delta.unsigned_abs())
        } else {
            self.grid_overview_cursor.saturating_add(delta as usize)
        };
        self.set_grid_overview_cursor(target, strategy, cx);
    }

    fn scroll_grid_overview_by(&mut self, direction: f32, fraction: f32, cx: &mut Context<Self>) {
        let scroll_handle = self.grid_overview_scroll.0.borrow().base_handle.clone();
        let offset = scroll_handle.offset();
        let max_offset = scroll_handle.max_offset();
        let delta = scroll_handle.bounds().size.height.as_f32() * fraction;
        let max_y = max_offset.y.as_f32().max(0.0);
        if max_y <= 0.0 || delta <= 0.0 {
            return;
        }

        let target_y = (offset.y.as_f32() - direction * delta).clamp(-max_y, 0.0);
        if (target_y - offset.y.as_f32()).abs() <= f32::EPSILON {
            return;
        }

        scroll_handle.set_offset(point(offset.x, px(target_y)));
        cx.notify();
    }

    fn scroll_grid_overview_to_top(&mut self, cx: &mut Context<Self>) {
        self.grid_overview_scroll
            .scroll_to_item_strict(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn scroll_grid_overview_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.grid_overview_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn confirm_grid_overview_selection(&mut self, cx: &mut Context<Self>) {
        let page_idx = self.grid_overview_cursor.min(self.last_page_index());
        self.navigate_to_pdf_destination(page_idx, None, None, cx);
        self.set_grid_overview_open(false, cx);
    }

    fn grid_cursor_up(&mut self, cx: &mut Context<Self>) {
        self.move_grid_overview_cursor(
            -(self.grid_overview_cols.max(1) as isize),
            ScrollStrategy::Center,
            cx,
        );
    }

    fn grid_cursor_down(&mut self, cx: &mut Context<Self>) {
        self.move_grid_overview_cursor(
            self.grid_overview_cols.max(1) as isize,
            ScrollStrategy::Center,
            cx,
        );
    }

    fn grid_cursor_left(&mut self, cx: &mut Context<Self>) {
        self.move_grid_overview_cursor(-1, ScrollStrategy::Center, cx);
    }

    fn grid_cursor_right(&mut self, cx: &mut Context<Self>) {
        self.move_grid_overview_cursor(1, ScrollStrategy::Center, cx);
    }

    fn next_page(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_by(1.0, 1.0, cx);
            return;
        }

        self.navigate_to_page_start(self.current_page_index() + 1, cx);
    }

    fn previous_page(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_by(-1.0, 1.0, cx);
            return;
        }

        self.navigate_to_page_start(self.current_page_index().saturating_sub(1), cx);
    }

    fn go_to_beginning(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_to_top(cx);
            return;
        }
        if self.is_presentation_mode() {
            if self.set_presentation_page(0, cx) {
                self.mark_manual_navigation();
            }
            return;
        }

        if self.set_scroll_doc_target(self.target_scroll_x_doc, 0.0, cx) {
            self.mark_manual_navigation();
        }
    }

    fn go_to_end(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_to_bottom(cx);
            return;
        }
        if self.is_presentation_mode() {
            if self.set_presentation_page(self.last_page_index(), cx) {
                self.mark_manual_navigation();
            }
            return;
        }

        if self.set_scroll_doc_target(self.target_scroll_x_doc, self.max_scroll_y_doc(), cx) {
            self.mark_manual_navigation();
        }
    }

    fn next_half_page(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_by(1.0, 0.5, cx);
            return;
        }

        self.scroll_half_page(1.0, cx);
    }

    fn previous_half_page(&mut self, cx: &mut Context<Self>) {
        if self.grid_overview_open {
            self.scroll_grid_overview_by(-1.0, 0.5, cx);
            return;
        }

        self.scroll_half_page(-1.0, cx);
    }

    fn scroll_half_page(&mut self, direction: f32, cx: &mut Context<Self>) {
        if self.is_presentation_mode() {
            self.navigate_presentation_by_direction(direction, cx);
            return;
        }

        let delta = self.viewport_height / (2.0 * self.effective_scale());
        if self.set_scroll_doc_target(
            self.target_scroll_x_doc,
            self.target_scroll_y_doc + direction * delta,
            cx,
        ) {
            self.mark_manual_navigation();
        }
    }

    fn scroll_line(&mut self, direction: f32, cx: &mut Context<Self>) {
        if self.is_presentation_mode() {
            self.navigate_presentation_by_direction(direction, cx);
            return;
        }

        let delta = SCROLL_LINE_STEP_PX / self.effective_scale();
        if self.set_scroll_doc_target(
            self.target_scroll_x_doc,
            self.target_scroll_y_doc + direction * delta,
            cx,
        ) {
            self.mark_manual_navigation();
        }
    }

    fn scroll_wheel_requests_zoom(event: &ScrollWheelEvent) -> bool {
        event.modifiers.control
    }

    fn handle_scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if Self::scroll_wheel_requests_zoom(event) {
            let zoom_delta = match event.delta {
                ScrollDelta::Pixels(pixels) => pixels.y.as_f32() / SCROLL_LINE_MULTIPLIER,
                ScrollDelta::Lines(lines) => lines.y,
            };
            if zoom_delta == 0.0 {
                return;
            }

            let factor = cx
                .global::<SettingsStore>()
                .settings
                .behavior
                .zoom_step
                .powf(zoom_delta * CTRL_SCROLL_ZOOM_STEP_MULTIPLIER);
            if self.grid_overview_open {
                self.set_grid_overview_zoom(self.grid_overview_zoom * factor, cx);
            } else if !self.is_presentation_mode() {
                self.set_zoom(self.zoom * factor, cx);
            }
        } else {
            let (delta_x, delta_y) = match event.delta {
                ScrollDelta::Pixels(pixels) => (pixels.x.as_f32(), pixels.y.as_f32()),
                ScrollDelta::Lines(lines) => (
                    lines.x * SCROLL_LINE_MULTIPLIER,
                    lines.y * SCROLL_LINE_MULTIPLIER,
                ),
            };
            if delta_x == 0.0 && delta_y == 0.0 {
                return;
            }
            if self.is_presentation_mode() {
                self.navigate_presentation_by_direction(-delta_y, cx);
                return;
            }

            let scale = self.effective_scale();
            if self.set_scroll_doc_target(
                self.target_scroll_x_doc - delta_x / scale,
                self.target_scroll_y_doc - delta_y / scale,
                cx,
            ) {
                self.mark_manual_navigation();
            }
        }
    }

    fn close_all_overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_quick_ask(window, cx);
        self.close_search_bar(window, cx);
        self.close_toc_palette(window, cx);
        self.close_notes_palette(window, cx);
        self.close_history_palette(window, cx);
        self.close_bookmark_palette(window, cx);
        self.close_bookmark_prompt(window, cx);
        self.close_go_to_page_prompt(window, cx);
        self.close_epub_style_panel(cx);
    }

    fn apply_transparency(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = cx.global::<SettingsStore>().settings.window;
        window.set_background_appearance(settings.background_appearance(self.transparency_enabled));
        cx.notify();
    }

    fn toggle_quick_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(quick_ask) = self.quick_ask.clone() {
            self.quick_ask_visible = !self.quick_ask_visible;
            if self.quick_ask_visible {
                quick_ask.focus_handle(cx).focus(window, cx);
            } else {
                self.focus_handle.focus(window, cx);
            }
            cx.notify();
            return;
        }

        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let document_path = self.path.clone();
        let page_state = self.ai_page_state();
        let context_provider = PdfViewAiContextProvider {
            pdf_view: cx.entity().downgrade(),
        };
        let width = self.viewport_width.max(320.0);
        let quick_ask = cx.new(|cx| {
            AiPanel::new_quick_ask(
                previous_focus_handle.clone(),
                context_provider.clone(),
                document_path.clone(),
                page_state,
                width,
                cx,
            )
        });

        cx.subscribe_in(
            &quick_ask,
            window,
            |this, _, event: &AiPanelEvent, window, cx| match event {
                AiPanelEvent::Dismissed => this.close_quick_ask(window, cx),
                AiPanelEvent::Changed => {}
            },
        )
        .detach();

        quick_ask.focus_handle(cx).focus(window, cx);
        self.quick_ask = Some(quick_ask);
        self.quick_ask_visible = true;
        cx.notify();
    }

    fn close_quick_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quick_ask_visible = false;
        if self.quick_ask.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn toggle_transparency(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.transparency_enabled = !self.transparency_enabled;
        self.apply_transparency(window, cx);
    }

    fn toggle_presentation_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.capabilities.presentation {
            return;
        }
        if let Some(page_idx) = self.presentation_page.take() {
            let scroll_y_doc = self.page_scroll_y_doc(page_idx);
            self.set_scroll_doc(self.scroll_x_doc, scroll_y_doc, cx);
            self.mark_manual_navigation();
            self.focus_handle.focus(window, cx);
            cx.notify();
            return;
        }

        let page_idx = self.current_page_index().min(self.last_page_index());
        self.close_all_overlays(window, cx);
        self.close_context_menu(cx);
        self.close_grid_overview_actions(cx);
        self.close_thumbnail_sidebar_actions(cx);
        self.set_grid_overview_open(false, cx);
        self.panning = None;
        self.dragging = false;
        self.pending_link = None;
        self.pending_selection_anchor = None;
        self.hovered_link = None;
        self.hovered_note = None;
        self.hovered_note_position = None;
        self.presentation_page = Some(page_idx);
        self.focus_handle.focus(window, cx);
        self.schedule_position_save(cx);
        cx.notify();
    }

    fn toggle_ai_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai_panel.is_some() {
            self.close_ai_panel(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);
        self.clamp_ai_panel_width(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let document_path = self.path.clone();
        let page_state = self.ai_page_state();
        let context_provider = PdfViewAiContextProvider {
            pdf_view: cx.entity().downgrade(),
        };
        let panel = cx.new(|cx| {
            AiPanel::new(
                previous_focus_handle.clone(),
                context_provider.clone(),
                document_path.clone(),
                page_state,
                self.ai_panel_width,
                cx,
            )
        });

        cx.subscribe_in(
            &panel,
            window,
            |this, _, event: &AiPanelEvent, window, cx| match event {
                AiPanelEvent::Dismissed => this.close_ai_panel(window, cx),
                AiPanelEvent::Changed => {}
            },
        )
        .detach();

        panel.focus_handle(cx).focus(window, cx);
        self.ai_panel = Some(panel);
        cx.notify();
    }

    fn close_ai_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.ai_panel.take() {
            panel.update(cx, |panel, cx| panel.save_current_session(cx));
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_bar.is_some() {
            self.close_search_bar(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let query = self.search.query.clone();
        let active = self.search.active;
        let match_count = self.search.matches.len();
        let ignore_case = self.search.ignore_case;
        let indexing = self.text_index_state.is_pending_or_indexing() && !query.is_empty();
        let search_bar = cx.new(|cx| {
            SearchBar::new(
                previous_focus_handle.clone(),
                query.clone(),
                active,
                match_count,
                ignore_case,
                indexing,
                cx,
            )
        });

        cx.subscribe_in(
            &search_bar,
            window,
            |this, _, event: &SearchEvent, window, cx| match event {
                SearchEvent::Activate { previous, query } => {
                    this.deploy_search_query(query.clone(), *previous, cx)
                }
                SearchEvent::QueryChanged { query } => this.live_search(query.clone(), cx),
                SearchEvent::IgnoreCaseChanged { query, ignore_case } => {
                    this.set_search_ignore_case(query.clone(), *ignore_case, cx)
                }
                SearchEvent::Dismissed => this.close_search_bar(window, cx),
            },
        )
        .detach();

        search_bar.focus_handle(cx).focus(window, cx);
        self.search_bar = Some(search_bar);
        cx.notify();
    }

    fn close_search_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_bar.take().is_some() {
            self.search.query = "".into();
            self.search.matches.clear();
            self.search.active = None;
            self.search.cancel_reveal_after_indexing();
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn toggle_toc_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.toc_sidebar_open {
            self.toc_sidebar_open = false;
            cx.notify();
            return;
        }

        self.close_left_sidebars_except(LeftSidebar::Toc);
        let width =
            Self::clamp_sidebar_width_for_window(self.toc_sidebar_width, TOC_SIDEBAR_WIDTH, window);
        if (self.toc_sidebar_width - width).abs() > f32::EPSILON {
            self.toc_sidebar_width = width;
            self.schedule_sidebar_width_save(cx);
        }
        self.toc_sidebar_open = true;
        cx.notify();
    }

    fn toggle_toc_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.toc_palette.is_some() {
            self.close_toc_palette(window, cx);
            return;
        }

        let entries = self.toc_palette_entries();
        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let selected_ix = self.current_toc_palette_match_ix(&entries);
        let delegate = TocPaletteDelegate::new(cx.entity().downgrade(), entries);
        let palette = cx.new(|cx| {
            Picker::new_with_selected_ix(previous_focus_handle.clone(), delegate, selected_ix, cx)
        });

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_toc_palette(window, cx);
        })
        .detach();

        palette.focus_handle(cx).focus(window, cx);
        self.toc_palette = Some(palette);
        cx.notify();
    }

    fn close_toc_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.toc_palette.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn toggle_notes_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notes_sidebar_open {
            self.notes_sidebar_open = false;
            cx.notify();
            return;
        }

        self.close_left_sidebars_except(LeftSidebar::Notes);
        self.refresh_notes_sidebar_entries();
        let width = Self::clamp_sidebar_width_for_window(
            self.notes_sidebar_width,
            NOTES_SIDEBAR_WIDTH,
            window,
        );
        if (self.notes_sidebar_width - width).abs() > f32::EPSILON {
            self.notes_sidebar_width = width;
            self.schedule_sidebar_width_save(cx);
        }
        self.notes_sidebar_open = true;
        cx.notify();
    }

    fn toggle_thumbnail_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.capabilities.page_overview {
            return;
        }
        self.set_thumbnail_sidebar_open(!self.thumbnail_sidebar_open, window, cx);
    }

    fn toggle_grid_overview(&mut self, cx: &mut Context<Self>) {
        if !self.capabilities.page_overview {
            return;
        }
        self.set_grid_overview_open(!self.grid_overview_open, cx);
    }

    fn toggle_notes_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notes_palette.is_some() {
            self.close_notes_palette(window, cx);
            return;
        }

        let entries = self.note_palette_entries();
        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let delegate = NotesPaletteDelegate::new(cx.entity().downgrade(), entries);
        let palette = cx.new(|cx| Picker::new(previous_focus_handle.clone(), delegate, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_notes_palette(window, cx);
        })
        .detach();

        palette.focus_handle(cx).focus(window, cx);
        self.notes_palette = Some(palette);
        cx.notify();
    }

    fn close_notes_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notes_palette.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn note_palette_entries(&self) -> Vec<NotePaletteEntry> {
        NotePaletteEntry::for_notes(&self.notes)
    }

    fn refresh_notes_sidebar_entries(&mut self) {
        let entries = self.note_palette_entries();
        if let Some(selected) = self.notes_sidebar_selected
            && !entries.iter().any(|entry| entry.id == selected)
        {
            self.notes_sidebar_selected = None;
        }
        self.notes_sidebar_entries = entries;
    }

    fn toggle_history_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_palette.is_some() {
            self.close_history_palette(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);

        let entries = cx.global::<LibraryStore>().history_entries().to_vec();
        let previous_focus_handle = self.focus_handle.clone();
        let delegate = HistoryPaletteDelegate::new(cx.entity().downgrade(), entries);
        let palette = cx.new(|cx| Picker::new(previous_focus_handle.clone(), delegate, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_history_palette(window, cx);
        })
        .detach();

        palette.focus_handle(cx).focus(window, cx);
        self.history_palette = Some(palette);
        cx.notify();
    }

    fn close_history_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_palette.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn toggle_bookmark_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bookmark_palette.is_some() {
            self.close_bookmark_palette(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);

        let bookmarks = cx.global::<LibraryStore>().bookmarks().to_vec();
        let previous_focus_handle = self.focus_handle.clone();
        let delegate =
            BookmarkPaletteDelegate::new(cx.entity().downgrade(), bookmarks, self.path.clone());
        let palette = cx.new(|cx| Picker::new(previous_focus_handle.clone(), delegate, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_bookmark_palette(window, cx);
        })
        .detach();

        palette.focus_handle(cx).focus(window, cx);
        self.bookmark_palette = Some(palette);
        cx.notify();
    }

    fn close_bookmark_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bookmark_palette.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn toggle_bookmark_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bookmark_prompt.is_some() {
            self.close_bookmark_prompt(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let default_label = self.default_bookmark_label();
        let delegate =
            BookmarkPromptDelegate::new(self.path.clone(), self.current_document_position());
        let prompt = cx.new(|cx| {
            Picker::with_content(previous_focus_handle.clone(), delegate, default_label, cx)
        });

        cx.subscribe_in(&prompt, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_bookmark_prompt(window, cx);
        })
        .detach();

        prompt.focus_handle(cx).focus(window, cx);
        self.bookmark_prompt = Some(prompt);
        cx.notify();
    }

    fn close_bookmark_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bookmark_prompt.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn toggle_go_to_page_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.capabilities.reflow {
            return;
        }
        if self.go_to_page_prompt.is_some() {
            self.close_go_to_page_prompt(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let delegate = GoToPagePromptDelegate::new(cx.entity().downgrade(), self.page_count());
        let current_page = self.current_page().to_string().into();
        let prompt = cx.new(|cx| {
            Picker::with_content(previous_focus_handle.clone(), delegate, current_page, cx)
        });

        cx.subscribe_in(&prompt, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_go_to_page_prompt(window, cx);
        })
        .detach();

        prompt.focus_handle(cx).focus(window, cx);
        self.go_to_page_prompt = Some(prompt);
        cx.notify();
    }

    fn close_go_to_page_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.go_to_page_prompt.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn default_bookmark_label(&self) -> SharedString {
        let current_page = self.current_page_index();
        self.toc
            .iter()
            .rfind(|entry| entry.navigable && entry.page_idx <= current_page)
            .map(|entry| entry.title.clone())
            .unwrap_or_else(|| crate::fl!("page-title", page = self.current_page() as i64).into())
    }

    fn toc_palette_entries(&self) -> Vec<TocPaletteEntry> {
        self.toc
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.navigable)
            .map(|(entry_ix, entry)| TocPaletteEntry {
                entry_ix,
                title: entry.title.clone(),
                depth: entry.depth,
                page_number: entry.page_idx + 1,
            })
            .collect()
    }

    fn current_toc_palette_match_ix(&self, entries: &[TocPaletteEntry]) -> usize {
        if let Some(selected) = self.toc_sidebar_selected
            && let Some(match_ix) = entries.iter().position(|entry| entry.entry_ix == selected)
        {
            return match_ix;
        }

        let current_page_number = self.current_page_index() + 1;
        entries
            .iter()
            .position(|entry| entry.page_number == current_page_number)
            .or_else(|| {
                entries
                    .iter()
                    .rposition(|entry| entry.page_number < current_page_number)
            })
            .unwrap_or(0)
    }

    fn navigate_to_pdf_destination(
        &mut self,
        page_idx: usize,
        target_top: Option<f32>,
        target_left: Option<f32>,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = self.pages.get(page_idx) else {
            return;
        };

        if self.is_presentation_mode() {
            let current_position = self.current_jump_position();
            Self::push_jump_position(&mut self.jump_back, current_position);
            self.jump_forward.clear();
            self.mark_manual_navigation();
            self.set_presentation_page(page_idx, cx);
            return;
        }

        let (page_width, _) = self.page_dimensions(page_idx);
        let target_point = Self::pdf_point_to_view(
            page.bounds,
            self.rotation,
            mupdf::Point {
                x: target_left.unwrap_or(page.bounds.x0 + page.width / 2.0),
                y: target_top.unwrap_or(page.bounds.y0),
            },
        );
        let swapped = self.rotation.swaps_width_and_height();
        let scroll_y_doc =
            if (swapped && target_left.is_some()) || (!swapped && target_top.is_some()) {
                page.offset_y + target_point.y
            } else {
                page.offset_y
            };
        let scroll_x_doc =
            if (swapped && target_top.is_some()) || (!swapped && target_left.is_some()) {
                target_point.x - page_width / 2.0
            } else {
                self.scroll_x_doc
            };

        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        self.jump_forward.clear();
        self.mark_manual_navigation();
        self.set_scroll_doc(scroll_x_doc, scroll_y_doc, cx);
    }

    pub(crate) fn navigate_to_page_number(&mut self, page_number: usize, cx: &mut Context<Self>) {
        if !(1..=self.page_count()).contains(&page_number) {
            return;
        }

        self.navigate_to_pdf_destination(page_number - 1, None, None, cx);
    }

    pub fn navigate_to_document_position(
        &mut self,
        position: DocumentPosition,
        cx: &mut Context<Self>,
    ) {
        if self.loading {
            self.pending_initial_position = Some(position);
            return;
        }

        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        self.jump_forward.clear();
        self.mark_manual_navigation();
        if self.capabilities.reflow && position.is_reflow() {
            self.restore_position(position);
            self.schedule_position_save(cx);
            cx.notify();
            return;
        }
        self.set_zoom(position.zoom, cx);
        self.set_scroll_doc(position.scroll_x_doc, position.scroll_y_doc, cx);
    }

    pub(crate) fn open_library_document(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(PdfViewEvent::OpenDocument { path, position });
    }

    pub(crate) fn navigate_to_toc_entry(&mut self, entry_ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.toc.get(entry_ix) else {
            return;
        };
        if !entry.navigable {
            return;
        }

        let page_idx = entry.page_idx;
        let target_top = entry.target_top;
        let target_left = entry.target_left;
        self.toc_sidebar_selected = Some(entry_ix);
        cx.notify();
        self.navigate_to_pdf_destination(page_idx, target_top, target_left, cx);
    }

    pub(crate) fn navigate_to_note(&mut self, note_ix: usize, cx: &mut Context<Self>) {
        let Some((note_id, page_idx, target_top)) = self.notes.get(note_ix).and_then(|note| {
            note.rects
                .iter()
                .min_by(|left, right| {
                    left.page_idx
                        .cmp(&right.page_idx)
                        .then_with(|| left.y0.total_cmp(&right.y0))
                        .then_with(|| left.x0.total_cmp(&right.x0))
                })
                .map(|rect| (note.id, rect.page_idx, rect.y0))
        }) else {
            return;
        };

        self.notes_sidebar_selected = Some(note_id);
        cx.notify();
        self.navigate_to_pdf_destination(page_idx, Some(target_top), None, cx);
    }

    fn activate_link(&mut self, link_ix: usize, cx: &mut Context<Self>) {
        let Some(target) = self.links.get(link_ix).map(|link| link.target.clone()) else {
            return;
        };

        match target {
            LinkTarget::Internal {
                page_idx,
                target_top,
                target_left,
            } => self.navigate_to_pdf_destination(page_idx, target_top, target_left, cx),
            LinkTarget::External(url) => cx.open_url(&url),
        }
    }

    fn open_context_menu(
        &mut self,
        position: GpuiPoint<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        self.dragging = false;
        self.pending_selection_anchor = None;
        self.panning = None;
        self.grid_overview_actions_position = None;
        self.thumbnail_sidebar_actions_position = None;
        self.context_menu_region =
            !self.grid_overview_open && self.point_in_selected_region(position);
        self.context_menu_image = if self.grid_overview_open
            || self.context_menu_region
            || self.context_menu_highlight.is_some()
            || self.context_menu_note.is_some()
        {
            None
        } else {
            self.image_at_screen(position, cx)
        };
        self.context_menu_copy_submenu_open = false;
        self.context_menu_position = Some(position);
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    fn close_context_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let was_open = self.context_menu_position.take().is_some();
        self.context_menu_page = None;
        self.context_menu_image = None;
        self.context_menu_highlight = None;
        self.context_menu_region = false;
        self.context_menu_copy_submenu_open = false;
        if was_open {
            self.context_menu_note = None;
            cx.notify();
        }
        was_open
    }

    fn context_menu_entry(
        label: String,
        theme: Theme,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(label.clone())
            .flex()
            .items_center()
            .h(px(30.0))
            .px_3()
            .text_size(ui_rems(13.0))
            .text_color(rgb(theme.text))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    handler(this, window, cx);
                    this.close_context_menu(cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn context_menu_copy_entry(
        theme: Theme,
        copy_submenu_open: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let submenu = div()
            .absolute()
            .left(px(188.0))
            .top_0()
            .w(px(190.0))
            .py_1()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .child(Self::context_menu_entry(
                crate::fl!("pdf-context-copy-plain-text"),
                theme,
                |this, _, cx| this.copy_selection_as(CopyFormat::PlainText, cx),
                cx,
            ))
            .child(Self::context_menu_entry(
                crate::fl!("pdf-context-copy-markdown"),
                theme,
                |this, _, cx| this.copy_selection_as(CopyFormat::Markdown, cx),
                cx,
            ));

        div()
            .relative()
            .when(copy_submenu_open, |this| this.child(submenu))
            .child(
                div()
                    .id("pdf-context-copy")
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(30.0))
                    .px_3()
                    .text_size(ui_rems(13.0))
                    .text_color(rgb(theme.text))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
                    .on_hover(cx.listener(|this, hovered, _, cx| {
                        if *hovered && !this.context_menu_copy_submenu_open {
                            this.context_menu_copy_submenu_open = true;
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.copy_selection(cx);
                            this.close_context_menu(cx);
                            window.prevent_default();
                            cx.stop_propagation();
                        }),
                    )
                    .child(crate::fl!("copy"))
                    .child("›"),
            )
    }

    fn context_menu_label(label: String, theme: Theme) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .h(px(24.0))
            .px_3()
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.text_muted))
            .child(label)
    }

    fn grid_overview_toolbar_button(
        id: &'static str,
        label: String,
        theme: Theme,
        handler: impl Fn(&mut Self, &MouseDownEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .h(px(24.0))
            .px_2()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgba(0x00000000))
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.text))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    handler(this, event, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn grid_overview_action_entry(
        label: String,
        theme: Theme,
        handler: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(label.clone())
            .flex()
            .items_center()
            .h(px(30.0))
            .px_3()
            .text_size(ui_rems(13.0))
            .text_color(rgb(theme.text))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    handler(this, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn render_page_selection_actions_menu(
        source: PageSelectionSource,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let text_source = source;
        let image_source = source;

        div()
            .occlude()
            .flex()
            .flex_col()
            .min_w(px(160.0))
            .py_1()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .child(Self::grid_overview_action_entry(
                crate::fl!("pdf-export-as-text"),
                theme,
                move |this, cx| this.export_selected_pages_as_text(text_source, cx),
                cx,
            ))
            .child(Self::grid_overview_action_entry(
                crate::fl!("pdf-export-as-image"),
                theme,
                move |this, cx| this.export_selected_pages_as_image(image_source, cx),
                cx,
            ))
    }

    fn render_grid_overview_actions_menu(theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        Self::render_page_selection_actions_menu(PageSelectionSource::GridOverview, theme, cx)
    }

    fn render_thumbnail_sidebar_actions_menu(
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Self::render_page_selection_actions_menu(PageSelectionSource::ThumbnailSidebar, theme, cx)
    }

    fn render_note_popup(body: SharedString, theme: Theme) -> impl IntoElement {
        let mut note = div()
            .occlude()
            .flex()
            .flex_col()
            .gap_1()
            .max_w(px(360.0))
            .p_3()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .text_size(ui_rems(13.0))
            .text_color(rgb(theme.text));

        for line in body.as_ref().lines() {
            note = note.child(div().child(line.to_string()));
        }

        note
    }

    fn render_centered_overlay(
        content: impl IntoElement + 'static,
        close: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(theme.overlay))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| close(this, window, cx)),
            )
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(content)
    }

    fn render_sidebar_resize_handle(
        id: &'static str,
        sidebar: DraggedSidebar,
        on_left: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + 'static {
        let offset = px(-SIDEBAR_RESIZE_HANDLE_SIZE / 2.0);
        let handle = div()
            .id(id)
            .on_drag(sidebar, |sidebar, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| *sidebar)
            })
            .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.close_context_menu(cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                }),
            )
            .absolute()
            .top(px(0.0))
            .h_full()
            .w(px(SIDEBAR_RESIZE_HANDLE_SIZE))
            .cursor_col_resize()
            .occlude();

        if on_left {
            deferred(handle.left(offset))
        } else {
            deferred(handle.right(offset))
        }
    }

    fn render_sidebar_shell(
        title: String,
        width: f32,
        header_end: impl IntoElement + 'static,
        content: impl IntoElement + 'static,
        resize_handle: impl IntoElement + 'static,
        theme: Theme,
        pdf_view: gpui::WeakEntity<Self>,
    ) -> impl IntoElement {
        div()
            .relative()
            .flex()
            .flex_none()
            .flex_col()
            .min_h_0()
            .w(px(width))
            .border_r(px(1.0))
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.text))
            .capture_any_mouse_down(move |_: &MouseDownEvent, _, cx| {
                let _ = pdf_view.update(cx, |this, cx| {
                    this.close_context_menu(cx);
                });
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .h(px(40.0))
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.border))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_rems(13.0))
                            .child(title),
                    )
                    .child(div().flex_none().child(header_end)),
            )
            .child(content)
            .child(resize_handle)
    }

    fn render_sidebar_count(count: usize, theme: Theme) -> impl IntoElement {
        div()
            .text_color(rgb(theme.text_muted))
            .child(crate::fl!("item-count", count = count as i64))
    }

    fn render_sidebar_empty_message(message: String, theme: Theme) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .px_3()
            .text_color(rgb(theme.text_muted))
            .child(message)
    }

    fn context_menu_highlight_color_row(
        action: HighlightColorAction,
        current: Option<HighlightColor>,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut row = div().flex().flex_row().gap_2().px_3().pb_2();

        for color in HighlightColor::ALL {
            row = row.child(Self::context_menu_highlight_swatch(
                color, current, action, theme, cx,
            ));
        }

        row
    }

    fn context_menu_highlight_swatch(
        color: HighlightColor,
        current: Option<HighlightColor>,
        action: HighlightColorAction,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(format!("highlight-swatch-{action:?}-{color:?}"))
            .w(px(22.0))
            .h(px(22.0))
            .border_1()
            .border_color(if current == Some(color) {
                rgb(theme.accent)
            } else {
                rgb(theme.border)
            })
            .bg(rgba(theme.highlight_color(color)))
            .cursor_pointer()
            .hover(move |style| style.border_color(rgb(theme.accent)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    match action {
                        HighlightColorAction::Create => {
                            this.create_highlight_from_selection(color, cx);
                        }
                        HighlightColorAction::Change => {
                            this.set_context_highlight_color(color, cx);
                        }
                    }
                    this.close_context_menu(cx);
                    cx.stop_propagation();
                }),
            )
    }

    fn context_menu_separator(theme: Theme) -> impl IntoElement {
        div().h(px(1.0)).my_1().mx_1().bg(rgb(theme.border))
    }

    fn render_context_menu(
        state: ContextMenuState,
        is_fullscreen: bool,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let fullscreen_label = if is_fullscreen {
            crate::fl!("pdf-context-exit-full-screen")
        } else {
            crate::fl!("pdf-context-enter-full-screen")
        };

        div()
            .occlude()
            .flex()
            .flex_col()
            .w(px(190.0))
            .py_1()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Middle, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Right, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .when(state.has_note, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("pdf-context-edit-note"),
                    theme,
                    |this, window, cx| this.begin_edit_context_note(window, cx),
                    cx,
                ))
                .child(Self::context_menu_entry(
                    crate::fl!("pdf-context-remove-note"),
                    theme,
                    |this, _, cx| this.remove_context_note(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(!state.has_note && state.has_selection, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("pdf-context-add-note"),
                    theme,
                    |this, window, cx| this.begin_add_note(window, cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when_some(state.highlight_color, |menu, color| {
                menu.child(Self::context_menu_label(
                    crate::fl!("pdf-context-change-highlight-color"),
                    theme,
                ))
                .child(Self::context_menu_highlight_color_row(
                    HighlightColorAction::Change,
                    Some(color),
                    theme,
                    cx,
                ))
                .child(Self::context_menu_entry(
                    crate::fl!("pdf-context-remove-highlight"),
                    theme,
                    |this, _, cx| this.remove_context_highlight(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(
                state.highlight_color.is_none() && state.has_selection,
                |menu| {
                    menu.child(Self::context_menu_label(
                        crate::fl!("pdf-context-highlight"),
                        theme,
                    ))
                    .child(Self::context_menu_highlight_color_row(
                        HighlightColorAction::Create,
                        None,
                        theme,
                        cx,
                    ))
                    .child(Self::context_menu_separator(theme))
                },
            )
            .when(state.has_image, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("pdf-context-copy-image"),
                    theme,
                    |this, _, cx| this.copy_image(cx),
                    cx,
                ))
                .child(Self::context_menu_entry(
                    crate::fl!("pdf-context-save-image"),
                    theme,
                    |this, _, cx| this.save_image(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(state.has_region, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("pdf-context-copy-region"),
                    theme,
                    |this, _, cx| this.copy_region(cx),
                    cx,
                ))
                .child(Self::context_menu_entry(
                    crate::fl!("pdf-context-save-region"),
                    theme,
                    |this, _, cx| this.save_region(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(state.has_page, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("pdf-context-save-page-as-image"),
                    theme,
                    |this, _, cx| this.save_page_as_image(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(
                state.highlight_color.is_none() && state.has_selection,
                |menu| {
                    menu.child(Self::context_menu_entry(
                        crate::fl!("pdf-context-translate-selection"),
                        theme,
                        |this, _, cx| {
                            let position = this.context_menu_position;
                            this.close_context_menu(cx);
                            this.translate_selection(position, false, cx);
                        },
                        cx,
                    ))
                    .child(Self::context_menu_copy_entry(
                        theme,
                        state.copy_submenu_open,
                        cx,
                    ))
                    .child(Self::context_menu_separator(theme))
                },
            )
            .when(!state.grid_overview, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("select-all"),
                    theme,
                    |this, _, cx| {
                        this.select_all();
                        cx.notify();
                    },
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .child(Self::context_menu_entry(
                crate::fl!("zoom-in"),
                theme,
                |this, _, cx| this.zoom_in(cx),
                cx,
            ))
            .child(Self::context_menu_entry(
                crate::fl!("zoom-out"),
                theme,
                |this, _, cx| this.zoom_out(cx),
                cx,
            ))
            .child(Self::context_menu_entry(
                crate::fl!("reset-zoom"),
                theme,
                |this, _, cx| this.reset_zoom(cx),
                cx,
            ))
            .child(Self::context_menu_separator(theme))
            .child(Self::context_menu_entry(
                fullscreen_label,
                theme,
                |_, window, _| window.toggle_fullscreen(),
                cx,
            ))
            .child(Self::context_menu_entry(
                crate::fl!("settings-title"),
                theme,
                |_, _, cx| cx.emit(PdfViewEvent::OpenSettings),
                cx,
            ))
    }

    fn ensure_toc_sidebar_visible_entries(&mut self) {
        if !self.toc_sidebar_entries_dirty {
            return;
        }

        self.toc_sidebar_visible_entry_ixs.clear();
        let mut hidden_depth = None;
        for (entry_ix, entry) in self.toc.iter().enumerate() {
            if let Some(depth) = hidden_depth {
                if entry.depth > depth {
                    continue;
                }
                hidden_depth = None;
            }

            self.toc_sidebar_visible_entry_ixs.push(entry_ix);
            if self.toc_collapsed.contains(&entry_ix) {
                hidden_depth = Some(entry.depth);
            }
        }
        self.toc_sidebar_entries_dirty = false;
    }

    fn render_footbar_icon_button(
        id: &'static str,
        icon: IconName,
        open: bool,
        theme: Theme,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(ui_rems(FOOTBAR_BUTTON_HEIGHT))
            .px_2()
            .rounded(px(4.0))
            .text_color(rgb(if open {
                theme.accent
            } else {
                theme.footer_text
            }))
            .bg(if open {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.close_epub_style_panel(cx);
                    handler(this, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(Icon::new(
                icon,
                ui_rems(FOOTBAR_ICON_SIZE),
                rgb(if open {
                    theme.accent
                } else {
                    theme.footer_text
                }),
            ))
    }

    fn render_toc_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        Self::render_footbar_icon_button(
            "footbar-toc",
            IconName::TableOfContents,
            self.toc_sidebar_open,
            theme,
            |this, window, cx| this.toggle_toc_sidebar(window, cx),
            cx,
        )
    }

    fn render_notes_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        Self::render_footbar_icon_button(
            "footbar-notes",
            IconName::NotebookTabs,
            self.notes_sidebar_open,
            theme,
            |this, window, cx| this.toggle_notes_sidebar(window, cx),
            cx,
        )
    }

    fn render_thumbnail_sidebar_button(
        &self,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Self::render_footbar_icon_button(
            "footbar-thumbnails",
            IconName::PanelLeft,
            self.thumbnail_sidebar_open,
            theme,
            |this, window, cx| this.toggle_thumbnail_sidebar(window, cx),
            cx,
        )
    }

    fn render_grid_overview_button(
        &self,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Self::render_footbar_icon_button(
            "footbar-grid-overview",
            IconName::LayoutGrid,
            self.grid_overview_open,
            theme,
            |this, _, cx| this.toggle_grid_overview(cx),
            cx,
        )
    }

    fn render_ai_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        Self::render_footbar_icon_button(
            "footbar-ai",
            IconName::Bot,
            self.ai_panel.is_some(),
            theme,
            |this, window, cx| this.toggle_ai_panel(window, cx),
            cx,
        )
    }

    fn toggle_region_selection(&mut self, cx: &mut Context<Self>) {
        if !self.capabilities.page_export || self.grid_overview_open {
            return;
        }
        self.region_tool_active = !self.region_tool_active;
        self.region_drag = None;
        if self.region_tool_active {
            self.selected_region = None;
            self.selection = None;
            self.selected_highlight = None;
        }
        self.close_context_menu(cx);
        cx.notify();
    }

    fn cancel_region_selection(&mut self, cx: &mut Context<Self>) {
        if !self.region_tool_active && self.region_drag.is_none() {
            return;
        }
        self.region_tool_active = false;
        self.region_drag = None;
        cx.notify();
    }

    fn dismiss_selected_region(&mut self, cx: &mut Context<Self>) {
        if self.selected_region.take().is_some() {
            self.context_menu_region = false;
            cx.notify();
        }
    }

    fn render_region_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("footbar-region-selection-tooltip")
            .child(Self::render_footbar_icon_button(
                "footbar-region-selection",
                IconName::Scan,
                self.region_tool_active,
                theme,
                |this, _, cx| this.toggle_region_selection(cx),
                cx,
            ))
            .tooltip(text_tooltip(crate::fl!("pdf-region-select-tooltip")))
    }

    fn render_region_popover(theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .occlude()
            .flex()
            .items_center()
            .gap_1()
            .p_1()
            .border_1()
            .border_color(rgb(theme.border))
            .rounded(px(6.0))
            .bg(rgb(theme.surface_background))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                IconButton::new(
                    "region-popover-copy",
                    IconName::Copy,
                    rgb(theme.text_muted),
                    rgb(theme.element_hover),
                )
                .into_div()
                .tooltip(text_tooltip(crate::fl!("pdf-context-copy-region")))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.copy_region(cx);
                        cx.stop_propagation();
                    }),
                ),
            )
            .child(
                IconButton::new(
                    "region-popover-save",
                    IconName::Download,
                    rgb(theme.text_muted),
                    rgb(theme.element_hover),
                )
                .into_div()
                .tooltip(text_tooltip(crate::fl!("pdf-context-save-region")))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.save_region(cx);
                        cx.stop_propagation();
                    }),
                ),
            )
            .child(
                IconButton::new(
                    "region-popover-dismiss",
                    IconName::X,
                    rgb(theme.text_muted),
                    rgb(theme.element_hover),
                )
                .into_div()
                .tooltip(text_tooltip(crate::fl!("pdf-region-dismiss")))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.dismiss_selected_region(cx);
                        cx.stop_propagation();
                    }),
                ),
            )
    }

    fn render_epub_control(
        id: &'static str,
        label: impl Into<SharedString>,
        active: bool,
        theme: Theme,
        handler: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .h(ui_rems(FOOTBAR_BUTTON_HEIGHT))
            .min_w(px(24.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .bg(if active {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if active {
                theme.accent
            } else {
                theme.footer_text
            }))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    handler(this, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label.into())
    }

    fn render_epub_stepper(
        config: EpubStepperConfig,
        decrease: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        increase: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(div().text_color(rgb(theme.text)).child(config.label))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(Self::render_epub_control(
                        config.decrease_id,
                        "−",
                        false,
                        theme,
                        decrease,
                        cx,
                    ))
                    .child(
                        div()
                            .min_w(px(54.0))
                            .text_center()
                            .text_color(rgb(theme.text_muted))
                            .child(config.value),
                    )
                    .child(Self::render_epub_control(
                        config.increase_id,
                        "+",
                        false,
                        theme,
                        increase,
                        cx,
                    )),
            )
    }

    fn render_epub_theme_option(
        id: &'static str,
        label: SharedString,
        option: EpubTheme,
        selected: EpubTheme,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Self::render_epub_control(
            id,
            label,
            option == selected,
            theme,
            move |this, cx| this.set_epub_theme(option, cx),
            cx,
        )
    }

    fn render_epub_style_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.epub_style_panel_position.is_some();
        div()
            .id("footbar-epub-style")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(ui_rems(FOOTBAR_BUTTON_HEIGHT))
            .px_2()
            .rounded(px(4.0))
            .text_color(rgb(if open {
                theme.accent
            } else {
                theme.footer_text
            }))
            .bg(if open {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.toggle_epub_style_panel(event.position, cx);
                    cx.stop_propagation();
                }),
            )
            .child(Icon::new(
                IconName::Settings,
                ui_rems(FOOTBAR_ICON_SIZE),
                rgb(if open {
                    theme.accent
                } else {
                    theme.footer_text
                }),
            ))
    }

    fn render_epub_style_panel(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let preferences = self.epub_preferences;
        div()
            .id("epub-style-panel")
            .occlude()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(340.0))
            .p_3()
            .border_1()
            .border_color(rgb(theme.border))
            .rounded(px(6.0))
            .bg(rgb(theme.surface_background))
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .text_size(ui_rems(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(crate::fl!("epub-appearance-title")),
            )
            .child(Self::render_epub_stepper(
                EpubStepperConfig {
                    label: crate::fl!("epub-font-size").into(),
                    value: format!("{:.0}", preferences.font_size),
                    decrease_id: "epub-font-decrease",
                    increase_id: "epub-font-increase",
                },
                |this, cx| this.decrease_epub_font_size(cx),
                |this, cx| this.increase_epub_font_size(cx),
                theme,
                cx,
            ))
            .child(Self::render_epub_stepper(
                EpubStepperConfig {
                    label: crate::fl!("epub-line-height").into(),
                    value: format!("{:.1}", preferences.line_height),
                    decrease_id: "epub-line-decrease",
                    increase_id: "epub-line-increase",
                },
                |this, cx| this.decrease_epub_line_height(cx),
                |this, cx| this.increase_epub_line_height(cx),
                theme,
                cx,
            ))
            .child(Self::render_epub_stepper(
                EpubStepperConfig {
                    label: crate::fl!("epub-horizontal-margin").into(),
                    value: format!("{:.0} px", preferences.horizontal_margin),
                    decrease_id: "epub-margin-decrease",
                    increase_id: "epub-margin-increase",
                },
                |this, cx| this.decrease_epub_horizontal_margin(cx),
                |this, cx| this.increase_epub_horizontal_margin(cx),
                theme,
                cx,
            ))
            .child(Self::render_epub_stepper(
                EpubStepperConfig {
                    label: crate::fl!("epub-max-content-width").into(),
                    value: format!("{:.0} px", preferences.max_content_width),
                    decrease_id: "epub-width-decrease",
                    increase_id: "epub-width-increase",
                },
                |this, cx| this.decrease_epub_max_content_width(cx),
                |this, cx| this.increase_epub_max_content_width(cx),
                theme,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_color(rgb(theme.text))
                            .child(crate::fl!("epub-theme")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(Self::render_epub_theme_option(
                                "epub-theme-light",
                                crate::fl!("epub-theme-light").into(),
                                EpubTheme::Light,
                                preferences.theme,
                                theme,
                                cx,
                            ))
                            .child(Self::render_epub_theme_option(
                                "epub-theme-sepia",
                                crate::fl!("epub-theme-sepia").into(),
                                EpubTheme::Sepia,
                                preferences.theme,
                                theme,
                                cx,
                            ))
                            .child(Self::render_epub_theme_option(
                                "epub-theme-dark",
                                crate::fl!("epub-theme-dark").into(),
                                EpubTheme::Dark,
                                preferences.theme,
                                theme,
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_color(rgb(theme.text))
                            .child(crate::fl!("epub-publisher-styles")),
                    )
                    .child(Self::render_epub_control(
                        "epub-publisher-styles",
                        if preferences.use_publisher_styles {
                            crate::fl!("epub-enabled")
                        } else {
                            crate::fl!("epub-disabled")
                        },
                        preferences.use_publisher_styles,
                        theme,
                        |this, cx| this.toggle_epub_publisher_styles(cx),
                        cx,
                    )),
            )
            .child(Self::render_epub_control(
                "epub-reset-defaults",
                crate::fl!("epub-reset-global-defaults"),
                false,
                theme,
                |this, cx| this.reset_epub_preferences(cx),
                cx,
            ))
    }

    fn selected_page_indices(&self, source: PageSelectionSource) -> Vec<usize> {
        let selection = match source {
            PageSelectionSource::GridOverview => &self.grid_overview_selection,
            PageSelectionSource::ThumbnailSidebar => &self.thumbnail_sidebar_selection,
        };

        selection
            .iter()
            .copied()
            .filter(|page_idx| *page_idx < self.pages.len())
            .collect()
    }

    fn cancel_page_selection(&mut self, source: PageSelectionSource, cx: &mut Context<Self>) {
        match source {
            PageSelectionSource::GridOverview => self.cancel_grid_overview_selection(cx),
            PageSelectionSource::ThumbnailSidebar => self.cancel_thumbnail_sidebar_selection(cx),
        }
    }

    fn enter_grid_overview_selection(&mut self, cx: &mut Context<Self>) {
        self.grid_overview_selecting = true;
        self.grid_overview_actions_position = None;
        self.close_context_menu(cx);
        cx.notify();
    }

    fn cancel_grid_overview_selection(&mut self, cx: &mut Context<Self>) {
        let changed = self.grid_overview_selecting
            || !self.grid_overview_selection.is_empty()
            || self.grid_overview_actions_position.is_some();
        self.grid_overview_selecting = false;
        self.grid_overview_selection.clear();
        self.grid_overview_actions_position = None;
        if changed {
            cx.notify();
        }
    }

    fn toggle_grid_overview_page_selection(&mut self, page_idx: usize, cx: &mut Context<Self>) {
        self.grid_overview_cursor = page_idx.min(self.last_page_index());
        if !self.grid_overview_selection.insert(page_idx) {
            self.grid_overview_selection.remove(&page_idx);
        }
        self.grid_overview_actions_position = None;
        cx.notify();
    }

    fn toggle_grid_overview_actions(
        &mut self,
        position: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        self.grid_overview_actions_position = if self.grid_overview_actions_position.is_some() {
            None
        } else {
            Some(position)
        };
        cx.notify();
    }

    fn close_grid_overview_actions(&mut self, cx: &mut Context<Self>) -> bool {
        let was_open = self.grid_overview_actions_position.take().is_some();
        if was_open {
            cx.notify();
        }
        was_open
    }

    fn enter_thumbnail_sidebar_selection(&mut self, cx: &mut Context<Self>) {
        self.thumbnail_sidebar_selecting = true;
        self.thumbnail_sidebar_actions_position = None;
        self.close_context_menu(cx);
        cx.notify();
    }

    fn cancel_thumbnail_sidebar_selection(&mut self, cx: &mut Context<Self>) {
        let changed = self.thumbnail_sidebar_selecting
            || !self.thumbnail_sidebar_selection.is_empty()
            || self.thumbnail_sidebar_actions_position.is_some();
        self.thumbnail_sidebar_selecting = false;
        self.thumbnail_sidebar_selection.clear();
        self.thumbnail_sidebar_actions_position = None;
        if changed {
            cx.notify();
        }
    }

    fn toggle_thumbnail_sidebar_page_selection(&mut self, page_idx: usize, cx: &mut Context<Self>) {
        if !self.thumbnail_sidebar_selection.insert(page_idx) {
            self.thumbnail_sidebar_selection.remove(&page_idx);
        }
        self.thumbnail_sidebar_actions_position = None;
        cx.notify();
    }

    fn toggle_thumbnail_sidebar_actions(
        &mut self,
        position: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        self.thumbnail_sidebar_actions_position =
            if self.thumbnail_sidebar_actions_position.is_some() {
                None
            } else {
                Some(position)
            };
        cx.notify();
    }

    fn close_thumbnail_sidebar_actions(&mut self, cx: &mut Context<Self>) -> bool {
        let was_open = self.thumbnail_sidebar_actions_position.take().is_some();
        if was_open {
            cx.notify();
        }
        was_open
    }

    fn set_thumbnail_sidebar_open(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.thumbnail_sidebar_open == open {
            return;
        }

        self.thumbnail_sidebar_open = open;
        if open {
            self.close_left_sidebars_except(LeftSidebar::Thumbnails);
            let width = Self::clamp_sidebar_width_for_window(
                self.thumbnail_sidebar_width,
                THUMBNAIL_SIDEBAR_WIDTH,
                window,
            );
            if (self.thumbnail_sidebar_width - width).abs() > f32::EPSILON {
                self.thumbnail_sidebar_width = width;
                self.schedule_sidebar_width_save(cx);
            }
            self.thumbnail_sidebar_current_page =
                self.current_page_index().min(self.last_page_index());
            if !self.pages.is_empty() {
                self.thumbnail_sidebar_scroll
                    .scroll_to_item(self.thumbnail_sidebar_current_page, ScrollStrategy::Center);
            }
        } else {
            self.thumbnail_sidebar_selecting = false;
            self.thumbnail_sidebar_selection.clear();
            self.thumbnail_sidebar_actions_position = None;
        }
        cx.notify();
    }

    fn sync_thumbnail_sidebar_current_page(&mut self) {
        if !self.thumbnail_sidebar_open || self.thumbnail_sidebar_selecting || self.pages.is_empty()
        {
            return;
        }

        let sync_scroll_y = if (self.target_scroll_y_doc - self.scroll_y_doc).abs() > f32::EPSILON {
            self.target_scroll_y_doc
        } else {
            self.scroll_y_doc
        };
        let current_page = self
            .page_index_for_scroll_y(sync_scroll_y)
            .min(self.last_page_index());
        if current_page == self.thumbnail_sidebar_current_page {
            return;
        }

        self.thumbnail_sidebar_current_page = current_page;
        self.thumbnail_sidebar_scroll
            .scroll_to_item(current_page, ScrollStrategy::Center);
    }

    fn set_grid_overview_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.grid_overview_open == open {
            return;
        }

        self.grid_overview_open = open;
        if open {
            self.region_tool_active = false;
            self.region_drag = None;
            self.selected_region = None;
            self.context_menu_region = false;
            self.grid_overview_cursor = self.current_page_index().min(self.last_page_index());
            self.scroll_grid_overview_cursor(ScrollStrategy::Center);
        } else {
            self.grid_overview_selecting = false;
            self.grid_overview_selection.clear();
            self.grid_overview_actions_position = None;
        }
        cx.notify();
    }

    /// Compute the available content width for the grid overview by subtracting
    /// open sidebar widths (including their 1px borders) from the window width.
    /// This allows column count to be computed synchronously during render,
    /// eliminating the one-frame delay from the measure-canvas approach.
    fn available_content_width(&self, window: &Window) -> f32 {
        let window_width = window.bounds().size.width.as_f32();
        let mut sidebar_total = 0.0f32;
        if self.thumbnail_sidebar_open {
            sidebar_total += self.thumbnail_sidebar_width + 1.0;
        }
        if self.toc_sidebar_open {
            sidebar_total += self.toc_sidebar_width + 1.0;
        }
        if self.notes_sidebar_open {
            sidebar_total += self.notes_sidebar_width + 1.0;
        }
        if self.ai_panel.is_some() {
            sidebar_total += self.ai_panel_width + 1.0;
        }
        (window_width - sidebar_total).max(0.0)
    }

    fn grid_overview_thumbnail_max_width(zoom: f32) -> f32 {
        GRID_OVERVIEW_BASE_THUMBNAIL_WIDTH * zoom
    }

    fn grid_overview_thumbnail_max_height(zoom: f32) -> f32 {
        GRID_OVERVIEW_BASE_THUMBNAIL_MAX_HEIGHT * zoom
    }

    fn grid_overview_cell_width(zoom: f32) -> f32 {
        Self::grid_overview_thumbnail_max_width(zoom) + GRID_OVERVIEW_CELL_PADDING
    }

    fn grid_overview_row_height(zoom: f32) -> f32 {
        Self::grid_overview_thumbnail_max_height(zoom)
            + GRID_OVERVIEW_CELL_PADDING
            + GRID_OVERVIEW_LABEL_HEIGHT
            + GRID_OVERVIEW_ROW_PADDING_Y
    }

    fn grid_overview_cols_for_width(viewport_width: f32, zoom: f32) -> usize {
        if !viewport_width.is_finite() || viewport_width <= 0.0 {
            return GRID_OVERVIEW_DEFAULT_COLS;
        }

        let cell_width = Self::grid_overview_cell_width(zoom).max(1.0);
        (((viewport_width - GRID_OVERVIEW_CELL_GAP) / (cell_width + GRID_OVERVIEW_CELL_GAP)).floor()
            as usize)
            .max(1)
    }

    fn grid_overview_horizontal_gap(viewport_width: f32, cols: usize, zoom: f32) -> f32 {
        if !viewport_width.is_finite() || viewport_width <= 0.0 {
            return GRID_OVERVIEW_CELL_GAP;
        }

        let cells_width = Self::grid_overview_cell_width(zoom) * cols as f32;
        ((viewport_width - cells_width) / (cols + 1) as f32).max(GRID_OVERVIEW_CELL_GAP)
    }

    fn update_grid_overview_cols(&mut self, viewport_width: f32, cx: &mut Context<Self>) -> bool {
        if !viewport_width.is_finite() || viewport_width <= 0.0 {
            return false;
        }

        let viewport_width_changed = (viewport_width - self.grid_overview_viewport_width).abs()
            > GRID_OVERVIEW_VIEWPORT_WIDTH_EPSILON;
        self.grid_overview_viewport_width = viewport_width;
        let cols = Self::grid_overview_cols_for_width(viewport_width, self.grid_overview_zoom);
        let cols_changed = cols != self.grid_overview_cols;
        if !cols_changed && !viewport_width_changed {
            return false;
        }

        self.grid_overview_cols = cols;
        if cols_changed {
            self.scroll_grid_overview_cursor(ScrollStrategy::Center);
        }
        cx.notify();
        true
    }

    fn thumbnail_sidebar_zoom(width: f32) -> f32 {
        let thumbnail_width = (width
            - THUMBNAIL_SIDEBAR_HORIZONTAL_PADDING * 2.0
            - THUMBNAIL_SIDEBAR_THUMBNAIL_PADDING)
            .clamp(
                THUMBNAIL_SIDEBAR_MIN_THUMBNAIL_WIDTH,
                THUMBNAIL_SIDEBAR_MAX_THUMBNAIL_WIDTH,
            );

        thumbnail_width / GRID_OVERVIEW_BASE_THUMBNAIL_WIDTH
    }

    fn thumbnail_sidebar_row_height(width: f32) -> f32 {
        let zoom = Self::thumbnail_sidebar_zoom(width);

        Self::grid_overview_thumbnail_max_height(zoom)
            + THUMBNAIL_SIDEBAR_THUMBNAIL_PADDING
            + THUMBNAIL_SIDEBAR_LABEL_HEIGHT
            + THUMBNAIL_SIDEBAR_ROW_PADDING_Y
    }

    fn grid_overview_thumbnail_size(&self, page_idx: usize, zoom: f32) -> (f32, f32) {
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let page_width = page_width.max(1.0);
        let page_height = page_height.max(1.0);
        let scale = (Self::grid_overview_thumbnail_max_width(zoom) / page_width)
            .min(Self::grid_overview_thumbnail_max_height(zoom) / page_height)
            .max(0.001);

        (page_width * scale, page_height * scale)
    }

    fn render_thumbnail_sidebar(
        &mut self,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.sync_thumbnail_sidebar_current_page();

        let page_count = self.pages.len();
        let selected_count = self
            .thumbnail_sidebar_selection
            .iter()
            .filter(|page_idx| **page_idx < page_count)
            .count();
        let page_export = self.capabilities.page_export;
        let selecting = page_export && self.thumbnail_sidebar_selecting;
        let zoom = Self::thumbnail_sidebar_zoom(self.thumbnail_sidebar_width);
        let row_height = Self::thumbnail_sidebar_row_height(self.thumbnail_sidebar_width);
        let thumbnail_max_width = Self::grid_overview_thumbnail_max_width(zoom);
        let thumbnail_max_height = Self::grid_overview_thumbnail_max_height(zoom);
        let thumbnail_box_width = thumbnail_max_width + THUMBNAIL_SIDEBAR_THUMBNAIL_PADDING;
        let thumbnail_box_height = thumbnail_max_height + THUMBNAIL_SIDEBAR_THUMBNAIL_PADDING;
        let scroll_handle = self.thumbnail_sidebar_scroll.clone();
        let actions_position = self.thumbnail_sidebar_actions_position;
        let compact_header =
            selecting && self.thumbnail_sidebar_width < THUMBNAIL_SIDEBAR_COMPACT_HEADER_WIDTH;
        let title = if compact_header {
            String::new()
        } else {
            crate::fl!("pdf-thumbnails-title")
        };

        let toolbar_label = if selecting && selected_count > 0 && !compact_header {
            Some(crate::fl!("selected-count", count = selected_count as i64))
        } else {
            None
        };

        let mut toolbar_actions = div().flex().items_center().gap_2();
        let actions_label = if compact_header {
            "⋯".to_string()
        } else {
            crate::fl!("actions-menu")
        };
        let cancel_label = if compact_header {
            "×".to_string()
        } else {
            crate::fl!("cancel")
        };
        if page_export {
            if selecting && selected_count > 0 {
                toolbar_actions = toolbar_actions
                    .child(Self::grid_overview_toolbar_button(
                        "thumbnail-sidebar-actions",
                        actions_label,
                        theme,
                        |this, event, _, cx| {
                            this.toggle_thumbnail_sidebar_actions(
                                point(event.position.x, event.position.y),
                                cx,
                            );
                        },
                        cx,
                    ))
                    .child(Self::grid_overview_toolbar_button(
                        "thumbnail-sidebar-cancel",
                        cancel_label,
                        theme,
                        |this, _, _, cx| this.cancel_thumbnail_sidebar_selection(cx),
                        cx,
                    ));
            } else if selecting {
                toolbar_actions = toolbar_actions.child(Self::grid_overview_toolbar_button(
                    "thumbnail-sidebar-cancel",
                    cancel_label,
                    theme,
                    |this, _, _, cx| this.cancel_thumbnail_sidebar_selection(cx),
                    cx,
                ));
            } else {
                toolbar_actions = toolbar_actions.child(Self::grid_overview_toolbar_button(
                    "thumbnail-sidebar-select",
                    crate::fl!("select"),
                    theme,
                    |this, _, _, cx| this.enter_thumbnail_sidebar_selection(cx),
                    cx,
                ));
            }
        }

        let header_end = div()
            .flex()
            .items_center()
            .gap_2()
            .when_some(toolbar_label, |this, label| {
                this.child(
                    div()
                        .text_color(rgb(theme.text_muted))
                        .truncate()
                        .child(label),
                )
            })
            .child(toolbar_actions);

        let content = div()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_thumbnail_sidebar_actions(cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when(page_count == 0, |this| {
                        this.child(Self::render_sidebar_empty_message(
                            crate::fl!("pdf-no-pages-available"),
                            theme,
                        ))
                    })
                    .when(page_count > 0, |this| {
                        this.child(
                            uniform_list(
                                "thumbnail-sidebar-list",
                                page_count,
                                cx.processor(
                                    move |this, visible_range: Range<usize>, _window, cx| {
                                        let theme =
                                            cx.global::<SettingsStore>().resolved_theme;
                                        let current_page =
                                            this.current_page_index().min(this.last_page_index());
                                        let selecting = this.thumbnail_sidebar_selecting;

                                        visible_range
                                            .filter_map(|page_idx| {
                                                this.pages.get(page_idx)?;
                                                let (thumbnail_width, thumbnail_height) =
                                                    this.grid_overview_thumbnail_size(page_idx, zoom);
                                                let current = page_idx == current_page;
                                                let selected = this
                                                    .thumbnail_sidebar_selection
                                                    .contains(&page_idx);
                                                let border_color = if selected || current {
                                                    theme.accent
                                                } else {
                                                    theme.border
                                                };
                                                let label_color = if selected || current {
                                                    theme.accent
                                                } else {
                                                    theme.text_muted
                                                };
                                                let thumbnail_view = cx.entity();
                                                let thumbnail = canvas(
                                                    {
                                                        let thumbnail_view =
                                                            thumbnail_view.clone();
                                                        move |bounds, window, cx| {
                                                            thumbnail_view.update(cx, |this, cx| {
                                                                let key = this.render_key(
                                                                    page_idx,
                                                                    bounds,
                                                                    window.scale_factor(),
                                                                );
                                                                this.cached_or_request_render(
                                                                    key, cx,
                                                                )
                                                            })
                                                        }
                                                    },
                                                    move |bounds, image, window, _| {
                                                        window.paint_quad(fill(
                                                            bounds,
                                                            rgb(theme.page),
                                                        ));
                                                        if let Some(image) = image {
                                                            let _ = window.paint_image(
                                                                bounds,
                                                                Corners::default(),
                                                                image,
                                                                0,
                                                                false,
                                                            );
                                                        }
                                                    },
                                                )
                                                .w(px(thumbnail_width))
                                                .h(px(thumbnail_height));

                                                Some(
                                                    div()
                                                        .id(("thumbnail-sidebar-page", page_idx))
                                                        .flex()
                                                        .flex_col()
                                                        .items_center()
                                                        .justify_center()
                                                        .w_full()
                                                        .h(px(row_height))
                                                        .px(px(THUMBNAIL_SIDEBAR_HORIZONTAL_PADDING))
                                                        .cursor_pointer()
                                                        .hover(move |style| {
                                                            style.bg(rgb(theme.element_hover))
                                                        })
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            cx.listener(move |this, _, _, cx| {
                                                                this.close_thumbnail_sidebar_actions(cx);
                                                                if this.thumbnail_sidebar_selecting {
                                                                    this.toggle_thumbnail_sidebar_page_selection(
                                                                        page_idx, cx,
                                                                    );
                                                                } else {
                                                                    this.navigate_to_pdf_destination(
                                                                        page_idx, None, None, cx,
                                                                    );
                                                                }
                                                                cx.stop_propagation();
                                                            }),
                                                        )
                                                        .on_mouse_down(
                                                            MouseButton::Right,
                                                            cx.listener(
                                                                move |this,
                                                                      event: &MouseDownEvent,
                                                                      window,
                                                                      cx| {
                                                                    this.close_thumbnail_sidebar_actions(cx);
                                                                    this.context_menu_page =
                                                                        Some(page_idx);
                                                                    this.context_menu_highlight =
                                                                        None;
                                                                    this.context_menu_note = None;
                                                                    this.selected_highlight = None;
                                                                    this.selection = None;
                                                                    this.open_context_menu(
                                                                        event.position,
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            ),
                                                        )
                                                        .child(
                                                            div()
                                                                .relative()
                                                                .flex()
                                                                .items_center()
                                                                .justify_center()
                                                                .w(px(thumbnail_box_width))
                                                                .h(px(thumbnail_box_height))
                                                                .border_1()
                                                                .border_color(rgb(border_color))
                                                                .bg(if selected {
                                                                    rgba(Self::with_alpha(
                                                                        theme.accent,
                                                                        0x22,
                                                                    ))
                                                                } else if current {
                                                                    rgba(Self::with_alpha(
                                                                        theme.accent,
                                                                        0x18,
                                                                    ))
                                                                } else {
                                                                    rgba(0x00000000)
                                                                })
                                                                .child(thumbnail)
                                                                .when(selecting, |this| {
                                                                    this.child(
                                                                        div()
                                                                            .absolute()
                                                                            .top(px(6.0))
                                                                            .right(px(6.0))
                                                                            .flex()
                                                                            .items_center()
                                                                            .justify_center()
                                                                            .w(px(22.0))
                                                                            .h(px(22.0))
                                                                            .border_1()
                                                                            .border_color(rgb(
                                                                                if selected {
                                                                                    theme.accent
                                                                                } else {
                                                                                    theme.border
                                                                                },
                                                                            ))
                                                                            .bg(rgba(
                                                                                Self::with_alpha(
                                                                                    theme
                                                                                        .surface_background,
                                                                                    0xe8,
                                                                                ),
                                                                            ))
                                                                            .text_size(ui_rems(13.0))
                                                                            .text_color(rgb(
                                                                                if selected {
                                                                                    theme.accent
                                                                                } else {
                                                                                    theme.text_muted
                                                                                },
                                                                            ))
                                                                            .child(if selected {
                                                                                "☑"
                                                                            } else {
                                                                                "☐"
                                                                            }),
                                                                    )
                                                                }),
                                                        )
                                                        .child(
                                                            div()
                                                                .mt_1()
                                                                .h(px(THUMBNAIL_SIDEBAR_LABEL_HEIGHT))
                                                                .text_size(ui_rems(11.0))
                                                                .text_color(rgb(label_color))
                                                                .child(format!("{}", page_idx + 1)),
                                                        ),
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                ),
                            )
                            .track_scroll(&scroll_handle)
                            .with_decoration(uniform_list_scrollbar_for(
                                &scroll_handle,
                                "thumbnail-sidebar-scrollbar",
                                window,
                                cx,
                            ))
                            .size_full(),
                        )
                    }),
            )
            .when_some(actions_position, |this, position| {
                this.child(
                    deferred(
                        anchored()
                            .snap_to_window_with_margin(px(8.0))
                            .position(position)
                            .anchor(Anchor::TopLeft)
                            .child(Self::render_thumbnail_sidebar_actions_menu(theme, cx)),
                    )
                    .with_priority(2),
                )
            });

        let pdf_view = cx.entity().downgrade();
        Self::render_sidebar_shell(
            title,
            self.thumbnail_sidebar_width,
            header_end,
            content,
            Self::render_sidebar_resize_handle(
                "thumbnail-sidebar-resize-handle",
                DraggedSidebar::Thumbnails,
                false,
                cx,
            ),
            theme,
            pdf_view,
        )
    }

    fn render_grid_overview(
        &mut self,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Compute available width synchronously from window bounds and sidebar
        // widths, eliminating the one-frame delay of the measure-canvas approach.
        let available_width = self.available_content_width(window);
        if available_width.is_finite() && available_width > 0.0 {
            let new_cols =
                Self::grid_overview_cols_for_width(available_width, self.grid_overview_zoom);
            let cols_changed = new_cols != self.grid_overview_cols;
            self.grid_overview_cols = new_cols;
            self.grid_overview_viewport_width = available_width;
            if cols_changed {
                self.scroll_grid_overview_cursor(ScrollStrategy::Center);
            }
        }

        let cols = self.grid_overview_cols.max(1);
        let rows = self.pages.len().div_ceil(cols);
        let zoom = self.grid_overview_zoom;
        let row_height = Self::grid_overview_row_height(zoom);
        let thumbnail_max_width = Self::grid_overview_thumbnail_max_width(zoom);
        let thumbnail_max_height = Self::grid_overview_thumbnail_max_height(zoom);
        let thumbnail_box_width = thumbnail_max_width + GRID_OVERVIEW_CELL_PADDING;
        let thumbnail_box_height = thumbnail_max_height + GRID_OVERVIEW_CELL_PADDING;
        let horizontal_gap =
            Self::grid_overview_horizontal_gap(self.grid_overview_viewport_width, cols, zoom);
        let actions_position = self.grid_overview_actions_position;
        let scroll_handle = self.grid_overview_scroll.clone();
        let measure_view = cx.entity();
        let page_export = self.capabilities.page_export;

        let measure = canvas(
            {
                let measure_view = measure_view.clone();
                move |bounds, _window, cx| {
                    let width = bounds.size.width.as_f32();
                    measure_view.update(cx, |this, cx| {
                        this.update_grid_overview_cols(width, cx);
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        div()
            .id("grid-overview")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .key_context("GridOverview")
            .flex()
            .flex_col()
            .bg(rgb(theme.surface_background))
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.text))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_context_menu(cx);
                    this.close_grid_overview_actions(cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.close_grid_overview_actions(cx);
                    let page_idx = this.grid_overview_cursor.min(this.last_page_index());
                    this.context_menu_page = Some(page_idx);
                    this.context_menu_highlight = None;
                    this.context_menu_note = None;
                    this.selected_highlight = None;
                    this.open_context_menu(event.position, window, cx);
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                if Self::scroll_wheel_requests_zoom(event) {
                    this.handle_scroll_wheel(event, cx);
                }
                cx.stop_propagation();
            }))
            .child(measure)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(GRID_OVERVIEW_HEADER_HEIGHT))
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.border))
                    .child(
                        div()
                            .text_size(ui_rems(13.0))
                            .child(crate::fl!("pdf-page-overview-title")),
                    )
                    .child(
                        {
                            let mut toolbar = div().flex().items_center().gap_2();
                            if page_export {
                                if self.grid_overview_selecting
                                    && !self.grid_overview_selection.is_empty()
                                {
                                    toolbar = toolbar
                                        .child(Self::grid_overview_toolbar_button(
                                            "grid-overview-actions",
                                            crate::fl!("actions-menu"),
                                            theme,
                                            |this, event, _, cx| {
                                                this.toggle_grid_overview_actions(
                                                    point(event.position.x, event.position.y),
                                                    cx,
                                                );
                                            },
                                            cx,
                                        ))
                                        .child(Self::grid_overview_toolbar_button(
                                            "grid-overview-cancel",
                                            crate::fl!("cancel-selection"),
                                            theme,
                                            |this, _, _, cx| {
                                                this.cancel_grid_overview_selection(cx)
                                            },
                                            cx,
                                        ));
                                } else if self.grid_overview_selecting {
                                    toolbar = toolbar.child(Self::grid_overview_toolbar_button(
                                        "grid-overview-cancel",
                                        crate::fl!("cancel"),
                                        theme,
                                        |this, _, _, cx| {
                                            this.cancel_grid_overview_selection(cx)
                                        },
                                        cx,
                                    ));
                                } else {
                                    toolbar = toolbar.child(Self::grid_overview_toolbar_button(
                                        "grid-overview-select",
                                        crate::fl!("select"),
                                        theme,
                                        |this, _, _, cx| {
                                            this.enter_grid_overview_selection(cx)
                                        },
                                        cx,
                                    ));
                                }
                            }
                            toolbar
                        },
                    ),
            )
            .child(
                div()
                    .id("grid-overview-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        uniform_list(
                            "grid-overview-list",
                            rows,
                            cx.processor(move |this, visible_range: Range<usize>, _window, cx| {
                                // Protect visible grid pages from cache eviction so
                                // thumbnails are not evicted by the pixel-budget LRU.
                                let protect_cols = this.grid_overview_cols.max(1);
                                let protect_page_count = this.pages.len();
                                let protect_doc_id = this.doc_id;
                                let protect_rotation = this.rotation;
                                let protected: Vec<(u64, usize, DocumentRotation)> = visible_range
                                    .clone()
                                    .flat_map(|row_ix| {
                                        let start = row_ix * protect_cols;
                                        let end =
                                            ((row_ix + 1) * protect_cols).min(protect_page_count);
                                        start..end
                                    })
                                    .map(|page_idx| {
                                        (protect_doc_id, page_idx, protect_rotation)
                                    })
                                    .collect();
                                cx.global_mut::<RenderCache>().set_protected_pages(protected);

                                let theme = cx.global::<SettingsStore>().resolved_theme;
                                let current_page =
                                    this.grid_overview_cursor.min(this.last_page_index());

                                visible_range
                                    .map(|row_ix| {
                                        let mut row = div()
                                            .flex()
                                            .flex_row()
                                            .items_start()
                                            .justify_center()
                                            .gap(px(horizontal_gap))
                                            .w_full()
                                            .h(px(row_height))
                                            .px(px(horizontal_gap))
                                            .py_2();

                                        for col_ix in 0..cols {
                                            let page_idx = row_ix * cols + col_ix;
                                            if page_idx < this.pages.len() {
                                                let (thumbnail_width, thumbnail_height) =
                                                    this.grid_overview_thumbnail_size(page_idx, zoom);
                                                let current = page_idx == current_page;
                                                let selecting = this.grid_overview_selecting;
                                                let selected = this
                                                    .grid_overview_selection
                                                    .contains(&page_idx);
                                                let border_color = if selected || current {
                                                    theme.accent
                                                } else {
                                                    theme.border
                                                };
                                                let label_color = if selected || current {
                                                    theme.accent
                                                } else {
                                                    theme.text_muted
                                                };
                                                let thumbnail_view = cx.entity();
                                                let thumbnail = canvas(
                                                    {
                                                        let thumbnail_view = thumbnail_view.clone();
                                                        move |bounds, window, cx| {
                                                            thumbnail_view.update(cx, |this, cx| {
                                                                let key = this.render_key(
                                                                    page_idx,
                                                                    bounds,
                                                                    window.scale_factor(),
                                                                );
                                                                this.cached_or_request_render(
                                                                    key, cx,
                                                                )
                                                            })
                                                        }
                                                    },
                                                    move |bounds, image, window, _| {
                                                        window.paint_quad(fill(
                                                            bounds,
                                                            rgb(theme.page),
                                                        ));
                                                        if let Some(image) = image {
                                                            let _ = window.paint_image(
                                                                bounds,
                                                                Corners::default(),
                                                                image,
                                                                0,
                                                                false,
                                                            );
                                                        }
                                                    },
                                                )
                                                .w(px(thumbnail_width))
                                                .h(px(thumbnail_height));

                                                row = row.child(
                                                    div()
                                                        .id(("grid-overview-page", page_idx))
                                                        .flex()
                                                        .flex_none()
                                                        .flex_col()
                                                        .items_center()
                                                        .relative()
                                                        .w(px(thumbnail_box_width))
                                                        .cursor_pointer()
                                                        .hover(move |style| {
                                                            style.bg(rgb(theme.element_hover))
                                                        })
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            cx.listener(move |this, _, _, cx| {
                                                                this.close_context_menu(cx);
                                                                this.close_grid_overview_actions(cx);
                                                                if this.grid_overview_selecting {
                                                                    this.toggle_grid_overview_page_selection(
                                                                        page_idx, cx,
                                                                    );
                                                                } else {
                                                                    this.navigate_to_pdf_destination(
                                                                        page_idx, None, None, cx,
                                                                    );
                                                                    this.set_grid_overview_open(
                                                                        false, cx,
                                                                    );
                                                                }
                                                                cx.stop_propagation();
                                                            }),
                                                        )
                                                        .on_mouse_down(
                                                            MouseButton::Right,
                                                            cx.listener(
                                                                move |this,
                                                                      event: &MouseDownEvent,
                                                                      window,
                                                                      cx| {
                                                                    this.close_grid_overview_actions(cx);
                                                                    this.grid_overview_cursor =
                                                                        page_idx;
                                                                    this.context_menu_page =
                                                                        Some(page_idx);
                                                                    this.context_menu_highlight =
                                                                        None;
                                                                    this.context_menu_note = None;
                                                                    this.selected_highlight = None;
                                                                    this.open_context_menu(
                                                                        event.position,
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            ),
                                                        )
                                                        .child(
                                                            div()
                                                                .relative()
                                                                .flex()
                                                                .items_center()
                                                                .justify_center()
                                                                .w(px(thumbnail_box_width))
                                                                .h(px(thumbnail_box_height))
                                                                .border_1()
                                                                .border_color(rgb(border_color))
                                                                .bg(if selected {
                                                                    rgba(Self::with_alpha(
                                                                        theme.accent,
                                                                        0x22,
                                                                    ))
                                                                } else if current {
                                                                    rgba(Self::with_alpha(
                                                                        theme.accent,
                                                                        0x18,
                                                                    ))
                                                                } else {
                                                                    rgba(0x00000000)
                                                                })
                                                                .child(thumbnail)
                                                                .when(selecting, |this| {
                                                                    this.child(
                                                                        div()
                                                                            .absolute()
                                                                            .top(px(6.0))
                                                                            .right(px(6.0))
                                                                            .flex()
                                                                            .items_center()
                                                                            .justify_center()
                                                                            .w(px(22.0))
                                                                            .h(px(22.0))
                                                                            .border_1()
                                                                            .border_color(rgb(
                                                                                if selected {
                                                                                    theme.accent
                                                                                } else {
                                                                                    theme.border
                                                                                },
                                                                            ))
                                                                            .bg(rgba(
                                                                                Self::with_alpha(
                                                                                    theme
                                                                                        .surface_background,
                                                                                    0xe8,
                                                                                ),
                                                                            ))
                                                                            .text_size(ui_rems(13.0))
                                                                            .text_color(rgb(
                                                                                if selected {
                                                                                    theme.accent
                                                                                } else {
                                                                                    theme.text_muted
                                                                                },
                                                                            ))
                                                                            .child(if selected {
                                                                                "☑"
                                                                            } else {
                                                                                "☐"
                                                                            }),
                                                                    )
                                                                }),
                                                        )
                                                        .child(
                                                            div()
                                                                .mt_1()
                                                                .text_size(ui_rems(11.0))
                                                                .text_color(rgb(label_color))
                                                                .child(format!("{}", page_idx + 1)),
                                                        ),
                                                );
                                            } else {
                                                row = row.child(
                                                    div().flex_none().w(px(thumbnail_box_width)),
                                                );
                                            }
                                        }

                                        row
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&scroll_handle)
                        .with_decoration(uniform_list_scrollbar_for(
                            &scroll_handle,
                            "grid-overview-scrollbar",
                            window,
                            cx,
                        ))
                        .size_full(),
                    ),
            )
            .when_some(actions_position, |this, position| {
                this.child(
                    deferred(
                        anchored()
                            .snap_to_window_with_margin(px(8.0))
                            .position(position)
                            .anchor(Anchor::TopLeft)
                            .child(Self::render_grid_overview_actions_menu(theme, cx)),
                    )
                    .with_priority(2),
                )
            })
    }

    fn render_toc_sidebar(
        &mut self,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.ensure_toc_sidebar_visible_entries();
        let visible_len = self.toc_sidebar_visible_entry_ixs.len();
        let scroll_handle = self.toc_sidebar_scroll.clone();

        let content = div()
            .id("toc-sidebar-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .when(visible_len == 0, |this| {
                this.child(Self::render_sidebar_empty_message(
                    crate::fl!("toc-empty"),
                    theme,
                ))
            })
            .when(visible_len > 0, |this| {
                this.child(
                    uniform_list(
                        "toc-sidebar-list",
                        visible_len,
                        cx.processor(|this, visible_range: Range<usize>, _window, cx| {
                            let theme = cx.global::<SettingsStore>().resolved_theme;
                            let selected = this.toc_sidebar_selected;

                            visible_range
                                .filter_map(|visible_ix| {
                                    let entry_ix = this
                                        .toc_sidebar_visible_entry_ixs
                                        .get(visible_ix)
                                        .copied()?;
                                    let entry = &this.toc[entry_ix];
                                    let collapsed = this.toc_collapsed.contains(&entry_ix);
                                    let current = selected == Some(entry_ix);
                                    let title = entry.title.clone();
                                    let title_color = if entry.navigable {
                                        theme.text
                                    } else {
                                        theme.text_muted
                                    };
                                    let depth = entry.depth;
                                    let has_children = entry.has_children;
                                    let navigable = entry.navigable;
                                    let page_number = entry.page_idx + 1;

                                    Some(
                                        div()
                                            .id(("toc-sidebar-entry", entry_ix))
                                            .flex()
                                            .flex_row()
                                            .w_full()
                                            .items_center()
                                            .h(px(TOC_SIDEBAR_ROW_HEIGHT))
                                            .px_2()
                                            .bg(if current {
                                                rgb(theme.element_selected)
                                            } else {
                                                rgba(0x00000000)
                                            })
                                            .when(navigable, |this| {
                                                this.cursor_pointer()
                                                    .hover(move |style| {
                                                        style.bg(rgb(theme.element_hover))
                                                    })
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(move |this, _, _, cx| {
                                                            this.navigate_to_toc_entry(
                                                                entry_ix, cx,
                                                            );
                                                            cx.stop_propagation();
                                                        }),
                                                    )
                                            })
                                            .child(div().flex_none().w(px(depth as f32 * 14.0)))
                                            .child(
                                                div()
                                                    .id(("toc-sidebar-disclosure", entry_ix))
                                                    .flex()
                                                    .flex_none()
                                                    .items_center()
                                                    .justify_center()
                                                    .w(px(18.0))
                                                    .h(px(24.0))
                                                    .text_color(rgb(theme.text_muted))
                                                    .when(has_children, |this| {
                                                        this.cursor_pointer()
                                                        .hover(move |style| {
                                                            style.bg(rgb(theme.element_hover))
                                                        })
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            cx.listener(move |this, _, _, cx| {
                                                                if !this
                                                                    .toc_collapsed
                                                                    .remove(&entry_ix)
                                                                {
                                                                    this.toc_collapsed
                                                                        .insert(entry_ix);
                                                                }
                                                                this.toc_sidebar_entries_dirty =
                                                                    true;
                                                                cx.stop_propagation();
                                                                cx.notify();
                                                            }),
                                                        )
                                                    })
                                                    .child(if has_children {
                                                        if collapsed { "▸" } else { "▾" }
                                                    } else {
                                                        ""
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .id(("toc-sidebar-title", entry_ix))
                                                    .flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .items_center()
                                                    .h(px(28.0))
                                                    .px_1()
                                                    .truncate()
                                                    .tooltip(text_tooltip(title.clone()))
                                                    .text_color(rgb(title_color))
                                                    .child(title),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .ml_2()
                                                    .w(px(SIDEBAR_PAGE_NUMBER_WIDTH))
                                                    .text_align(gpui::TextAlign::Right)
                                                    .text_color(rgb(theme.text_muted))
                                                    .child(if navigable {
                                                        format!("{}", page_number)
                                                    } else {
                                                        String::new()
                                                    }),
                                            ),
                                    )
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&scroll_handle)
                    .with_decoration(uniform_list_scrollbar_for(
                        &scroll_handle,
                        "toc-sidebar-scrollbar",
                        window,
                        cx,
                    ))
                    .size_full(),
                )
            });

        let pdf_view = cx.entity().downgrade();
        Self::render_sidebar_shell(
            crate::fl!("table-of-contents-title"),
            self.toc_sidebar_width,
            Self::render_sidebar_count(self.toc.len(), theme),
            content,
            Self::render_sidebar_resize_handle(
                "toc-sidebar-resize-handle",
                DraggedSidebar::Toc,
                false,
                cx,
            ),
            theme,
            pdf_view,
        )
    }

    fn render_notes_sidebar(
        &mut self,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let visible_len = self.notes_sidebar_entries.len();
        let scroll_handle = self.notes_sidebar_scroll.clone();
        let content = div()
            .id("notes-sidebar-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .when(visible_len == 0, |this| {
                this.child(Self::render_sidebar_empty_message(
                    crate::fl!("notes-empty"),
                    theme,
                ))
            })
            .when(visible_len > 0, |this| {
                this.child(
                    uniform_list(
                        "notes-sidebar-list",
                        visible_len,
                        cx.processor(|this, visible_range: Range<usize>, _window, cx| {
                            let theme = cx.global::<SettingsStore>().resolved_theme;
                            let selected = this.notes_sidebar_selected;

                            visible_range
                                .filter_map(|visible_ix| {
                                    let entry = this.notes_sidebar_entries.get(visible_ix)?;
                                    let note_ix = entry.note_ix;
                                    let note_id = entry.id;
                                    let body = entry.body.clone();
                                    let text = entry.text.clone();
                                    let page = entry.page;
                                    let color = entry.color;
                                    let current = selected == Some(note_id);

                                    Some(
                                        div()
                                            .id(format!("notes-sidebar-note-{note_id}"))
                                            .flex()
                                            .flex_row()
                                            .w_full()
                                            .items_center()
                                            .h(px(NOTES_SIDEBAR_ROW_HEIGHT))
                                            .px_2()
                                            .gap_2()
                                            .bg(if current {
                                                rgb(theme.element_selected)
                                            } else {
                                                rgba(0x00000000)
                                            })
                                            .cursor_pointer()
                                            .hover(move |style| style.bg(rgb(theme.element_hover)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    this.navigate_to_note(note_ix, cx);
                                                    cx.stop_propagation();
                                                }),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .w(px(8.0))
                                                    .h(px(8.0))
                                                    .bg(rgba(theme.note_color(color))),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex_col()
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_size(ui_rems(13.0))
                                                            .text_color(rgb(theme.text))
                                                            .child(body),
                                                    )
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_size(ui_rems(11.0))
                                                            .text_color(rgb(theme.text_muted))
                                                            .child(text),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .ml_2()
                                                    .w(px(SIDEBAR_PAGE_NUMBER_WIDTH))
                                                    .text_align(gpui::TextAlign::Right)
                                                    .text_color(rgb(theme.text_muted))
                                                    .child(format!("{}", page)),
                                            ),
                                    )
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&scroll_handle)
                    .with_decoration(uniform_list_scrollbar_for(
                        &scroll_handle,
                        "notes-sidebar-scrollbar",
                        window,
                        cx,
                    ))
                    .size_full(),
                )
            });

        let pdf_view = cx.entity().downgrade();
        Self::render_sidebar_shell(
            crate::fl!("notes-title"),
            self.notes_sidebar_width,
            Self::render_sidebar_count(visible_len, theme),
            content,
            Self::render_sidebar_resize_handle(
                "notes-sidebar-resize-handle",
                DraggedSidebar::Notes,
                false,
                cx,
            ),
            theme,
            pdf_view,
        )
    }

    fn highlight_overlays(&self) -> (Vec<HighlightOverlay>, Vec<Bounds<Pixels>>) {
        let mut overlays = Vec::new();
        let mut selected_borders = Vec::new();

        for highlight in &self.highlights {
            for rect in &highlight.rects {
                let Some(bounds) = self.highlight_rect_to_screen(rect) else {
                    continue;
                };
                overlays.push((bounds, highlight.color));
                if self.selected_highlight == Some(highlight.id) {
                    selected_borders.push(bounds);
                }
            }
        }

        (overlays, selected_borders)
    }

    fn note_underlines(&self) -> Vec<(Bounds<Pixels>, HighlightColor)> {
        let mut underlines = Vec::new();

        for note in &self.notes {
            for rect in &note.rects {
                let Some(bounds) = self.highlight_rect_to_screen(rect) else {
                    continue;
                };
                underlines.push((bounds, note.color));
            }
        }

        underlines
    }

    fn render_loading_state(
        &self,
        theme: Theme,
        opacity: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content = if let Some(error) = &self.loading_error {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .text_align(gpui::TextAlign::Center)
                .child(
                    div()
                        .text_size(ui_rems(15.0))
                        .text_color(rgb(theme.error))
                        .child(crate::fl!("pdf-failed-to-load")),
                )
                .child(
                    div()
                        .max_w(px(520.0))
                        .text_size(ui_rems(12.0))
                        .text_color(rgb(theme.text_muted))
                        .child(error.clone()),
                )
                .into_any_element()
        } else {
            const GRID_SQUARE_SIZE: f32 = 18.0;
            const GRID_GAP: f32 = 4.0;
            const GRID_PHASES: [f32; 9] = [0.80, 0.05, 0.50, 0.30, 0.95, 0.15, 0.65, 0.40, 0.70];
            const GRID_DURATION_MS: u64 = 1200;

            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .text_align(gpui::TextAlign::Center)
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(GRID_GAP))
                        .w(px(GRID_SQUARE_SIZE * 3.0 + GRID_GAP * 3.0))
                        .items_center()
                        .justify_center()
                        .children((0..9).map(|index| {
                            let phase = GRID_PHASES[index];

                            div()
                                .w(px(GRID_SQUARE_SIZE))
                                .h(px(GRID_SQUARE_SIZE))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .w(px(GRID_SQUARE_SIZE))
                                        .h(px(GRID_SQUARE_SIZE))
                                        .bg(rgb(theme.accent))
                                        .with_animation(
                                            ("pdf-grid-loader-square", index),
                                            Animation::new(Duration::from_millis(GRID_DURATION_MS))
                                                .repeat(),
                                            move |element, delta| {
                                                let t = (delta + phase) % 1.0;
                                                let wave =
                                                    (1.0 - (std::f32::consts::TAU * t).cos()) / 2.0;

                                                element.opacity(0.2 + 0.8 * wave)
                                            },
                                        ),
                                )
                        })),
                )
                .child(
                    div()
                        .text_size(ui_rems(14.0))
                        .text_color(rgb(theme.text_muted))
                        .child(crate::fl!("pdf-loading", name = self.file_name.as_ref())),
                )
                .child(
                    div()
                        .text_size(ui_rems(12.0))
                        .text_color(rgb(theme.text_muted))
                        .child(format!("{} pages", self.page_count)),
                )
                .into_any_element()
        };

        div()
            .size_full()
            .key_context("PdfView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Reload, _, cx| {
                this.request_reload(cx);
            }))
            .flex()
            .flex_col()
            .bg(rgb(theme.tab_active_background))
            .when(opacity < 1.0, |this| this.opacity(opacity))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_6()
                    .child(content),
            )
            .into_any_element()
    }

    pub fn has_global_chrome(&self) -> bool {
        !self.is_presentation_mode() && !self.loading && self.loading_error.is_none()
    }

    pub fn render_workspace_body_with_chrome(
        &mut self,
        center: AnyElement,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .size_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(theme.tab_active_background))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .on_drag_move(
                        cx.listener(|this, e: &DragMoveEvent<DraggedSidebar>, _, cx| {
                            let dragged_sidebar = *e.drag(cx);
                            let max_width = (e.bounds.size.width.as_f32()
                                * SIDEBAR_MAX_WIDTH_RATIO)
                                .max(SIDEBAR_MIN_WIDTH);
                            let new_width = match dragged_sidebar {
                                DraggedSidebar::Thumbnails => {
                                    (e.event.position.x - e.bounds.left()).as_f32()
                                }
                                DraggedSidebar::Toc => {
                                    let thumbnail_offset = if this.thumbnail_sidebar_open {
                                        this.thumbnail_sidebar_width
                                    } else {
                                        0.0
                                    };
                                    (e.event.position.x - e.bounds.left()).as_f32()
                                        - thumbnail_offset
                                }
                                DraggedSidebar::Notes => {
                                    let thumbnail_offset = if this.thumbnail_sidebar_open {
                                        this.thumbnail_sidebar_width
                                    } else {
                                        0.0
                                    };
                                    let toc_offset = if this.toc_sidebar_open {
                                        this.toc_sidebar_width
                                    } else {
                                        0.0
                                    };
                                    (e.event.position.x - e.bounds.left()).as_f32()
                                        - thumbnail_offset
                                        - toc_offset
                                }
                                DraggedSidebar::AiPanel => {
                                    (e.bounds.right() - e.event.position.x).as_f32()
                                }
                            }
                            .clamp(SIDEBAR_MIN_WIDTH, max_width);

                            match dragged_sidebar {
                                DraggedSidebar::Thumbnails => {
                                    this.thumbnail_sidebar_width = new_width;
                                }
                                DraggedSidebar::Toc => {
                                    this.toc_sidebar_width = new_width;
                                }
                                DraggedSidebar::Notes => {
                                    this.notes_sidebar_width = new_width;
                                }
                                DraggedSidebar::AiPanel => {
                                    this.ai_panel_width = new_width;
                                    if let Some(panel) = &this.ai_panel {
                                        panel.update(cx, |panel, cx| {
                                            panel.set_width(new_width);
                                            cx.notify();
                                        });
                                    }
                                }
                            }
                            this.schedule_sidebar_width_save(cx);
                            cx.notify();
                        }),
                    )
                    .when(self.thumbnail_sidebar_open, |this| {
                        this.child(self.render_thumbnail_sidebar(theme, window, cx))
                    })
                    .when(self.toc_sidebar_open, |this| {
                        this.child(self.render_toc_sidebar(theme, window, cx))
                    })
                    .when(self.notes_sidebar_open, |this| {
                        this.child(self.render_notes_sidebar(theme, window, cx))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .child(center),
                    )
                    .when_some(self.ai_panel.clone(), |this, panel| {
                        this.child(
                            div()
                                .relative()
                                .flex()
                                .flex_none()
                                .min_h_0()
                                .h_full()
                                .w(px(self.ai_panel_width))
                                .child(panel)
                                .child(Self::render_sidebar_resize_handle(
                                    "ai-panel-resize-handle",
                                    DraggedSidebar::AiPanel,
                                    true,
                                    cx,
                                )),
                        )
                    }),
            )
            .child(self.render_workspace_footbar(theme, cx))
            .into_any_element()
    }

    pub fn render_workspace_footbar(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let background_activity = NotificationCenter::try_global(cx)
            .and_then(|center| center.update(cx, |center, cx| center.render_activity(theme, cx)));
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .w_full()
            .flex_none()
            .h(ui_rems(FOOTBAR_HEIGHT))
            .px_3()
            .border_t(px(1.0))
            .border_color(rgb(theme.border))
            .bg(rgb(theme.tab_bar_background))
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.footer_text))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.render_toc_button(theme, cx))
                    .child(self.render_notes_button(theme, cx))
                    .when(self.capabilities.page_overview, |this| {
                        this.child(self.render_thumbnail_sidebar_button(theme, cx))
                            .child(self.render_grid_overview_button(theme, cx))
                    })
                    .when(
                        self.capabilities.page_export && !self.grid_overview_open,
                        |this| this.child(self.render_region_button(theme, cx)),
                    )
                    .when(self.capabilities.reflow, |this| {
                        this.child(self.render_epub_style_button(theme, cx))
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when_some(background_activity, |this, activity| this.child(activity))
                    .child(Self::page_counter_label(
                        self.current_page(),
                        self.pages.len(),
                    ))
                    .child(self.render_ai_button(theme, cx)),
            )
            .into_any_element()
    }

    fn highlight_rect_to_screen(&self, rect: &HighlightRect) -> Option<Bounds<Pixels>> {
        let image_bounds = *self.page_image_bounds.get(rect.page_idx)?.as_ref()?;
        Some(self.pdf_rect_to_screen(
            rect.page_idx,
            Rect::new(rect.x0, rect.y0, rect.x1, rect.y1),
            image_bounds,
        ))
    }

    fn selection_highlights(&self) -> Vec<Bounds<Pixels>> {
        let Some((start, end)) = self.selection_bounds() else {
            return Vec::new();
        };

        self.highlight_bounds_for_range(start..end + 1)
    }

    fn external_block_highlights(&self, block_id: Option<u32>) -> Vec<Bounds<Pixels>> {
        let Some(block_id) = block_id else {
            return Vec::new();
        };
        let Some(block) = self.text_blocks.iter().find(|block| block.id == block_id) else {
            return Vec::new();
        };
        self.highlight_bounds_for_range(block.glyph_range.clone())
    }

    fn link_highlights(&self) -> Vec<Bounds<Pixels>> {
        let Some(link_ix) = self.hovered_link else {
            return Vec::new();
        };
        let Some(link) = self.links.get(link_ix) else {
            return Vec::new();
        };

        let mut highlights = Vec::new();
        for rect in &link.rects {
            self.push_highlight(&mut highlights, link.page_idx, *rect);
        }

        highlights
    }

    fn search_highlights(&self) -> (Vec<Bounds<Pixels>>, Vec<Bounds<Pixels>>) {
        let mut highlights = Vec::new();
        let mut active_highlights = Vec::new();

        for (match_ix, search_match) in self.search.matches.iter().enumerate() {
            let bounds = self.highlight_bounds_for_range(search_match.range.clone());
            if self.search.active == Some(match_ix) {
                active_highlights.extend(bounds);
            } else {
                highlights.extend(bounds);
            }
        }

        (highlights, active_highlights)
    }

    fn highlight_rects_for_range(&self, range: Range<usize>) -> Vec<HighlightRect> {
        if range.start >= range.end || range.end > self.glyphs.len() {
            return Vec::new();
        }

        let mut rects = Vec::new();
        let mut current_page = None;
        let mut current_line = None;
        let mut current_rect = None;

        for glyph in &self.glyphs[range] {
            let rect = Self::glyph_line_rect(glyph);

            match (current_page, current_line, current_rect) {
                (Some(page), Some(line), Some(existing))
                    if page == glyph.page_idx && line == glyph.line_idx =>
                {
                    current_rect = Some(Self::union_rect(existing, rect));
                }
                (Some(page), Some(_), Some(existing)) => {
                    Self::push_highlight_rect(&mut rects, page, existing);
                    current_page = Some(glyph.page_idx);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
                _ => {
                    current_page = Some(glyph.page_idx);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
            }
        }

        if let (Some(page), Some(rect)) = (current_page, current_rect) {
            Self::push_highlight_rect(&mut rects, page, rect);
        }

        rects
    }

    fn highlight_bounds_for_range(&self, range: Range<usize>) -> Vec<Bounds<Pixels>> {
        if range.start >= range.end || range.end > self.glyphs.len() {
            return Vec::new();
        }

        let mut highlights = Vec::new();
        let mut current_page = None;
        let mut current_line = None;
        let mut current_rect = None;

        for glyph in &self.glyphs[range] {
            let rect = Self::glyph_line_rect(glyph);

            match (current_page, current_line, current_rect) {
                (Some(page), Some(line), Some(existing))
                    if page == glyph.page_idx && line == glyph.line_idx =>
                {
                    current_rect = Some(Self::union_rect(existing, rect));
                }
                (Some(page), Some(_), Some(existing)) => {
                    self.push_highlight(&mut highlights, page, existing);
                    current_page = Some(glyph.page_idx);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
                _ => {
                    current_page = Some(glyph.page_idx);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
            }
        }

        if let (Some(page), Some(rect)) = (current_page, current_rect) {
            self.push_highlight(&mut highlights, page, rect);
        }

        highlights
    }

    fn push_highlight(&self, highlights: &mut Vec<Bounds<Pixels>>, page_idx: usize, rect: Rect) {
        let Some(Some(image_bounds)) = self.page_image_bounds.get(page_idx) else {
            return;
        };

        highlights.push(self.pdf_rect_to_screen(page_idx, rect, *image_bounds));
    }

    fn push_highlight_rect(rects: &mut Vec<HighlightRect>, page_idx: usize, rect: Rect) {
        let Some(rect) = Self::normalized_rect(rect) else {
            return;
        };

        rects.push(HighlightRect {
            page_idx,
            x0: rect.x0,
            y0: rect.y0,
            x1: rect.x1,
            y1: rect.y1,
        });
    }

    fn pdf_rect_to_screen(
        &self,
        page_idx: usize,
        rect: Rect,
        image_bounds: Bounds<Pixels>,
    ) -> Bounds<Pixels> {
        let page = &self.pages[page_idx];
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let scale_x = image_bounds.size.width.as_f32() / page_width;
        let scale_y = image_bounds.size.height.as_f32() / page_height;
        let rect = Self::pdf_rect_to_view(page.bounds, self.rotation, rect);
        let x0 = image_bounds.origin.x.as_f32() + rect.x0 * scale_x;
        let y0 = image_bounds.origin.y.as_f32() + rect.y0 * scale_y;
        let x1 = image_bounds.origin.x.as_f32() + rect.x1 * scale_x;
        let y1 = image_bounds.origin.y.as_f32() + rect.y1 * scale_y;

        Bounds::new(
            point(px(x0), px(y0)),
            size(px((x1 - x0).max(1.0)), px((y1 - y0).max(1.0))),
        )
    }

    fn quad_rect(quad: &Quad) -> Rect {
        Rect::new(
            quad.ul.x.min(quad.ur.x).min(quad.ll.x).min(quad.lr.x),
            quad.ul.y.min(quad.ur.y).min(quad.ll.y).min(quad.lr.y),
            quad.ul.x.max(quad.ur.x).max(quad.ll.x).max(quad.lr.x),
            quad.ul.y.max(quad.ur.y).max(quad.ll.y).max(quad.lr.y),
        )
    }

    fn glyph_line_rect(glyph: &Glyph) -> Rect {
        let rect = Self::quad_rect(&glyph.quad);
        Rect::new(rect.x0, glyph.line_rect.y0, rect.x1, glyph.line_rect.y1)
    }

    fn union_rect(a: Rect, b: Rect) -> Rect {
        Rect::new(
            a.x0.min(b.x0),
            a.y0.min(b.y0),
            a.x1.max(b.x1),
            a.y1.max(b.y1),
        )
    }

    fn normalized_rect(rect: Rect) -> Option<Rect> {
        let x0 = rect.x0.min(rect.x1);
        let y0 = rect.y0.min(rect.y1);
        let x1 = rect.x0.max(rect.x1);
        let y1 = rect.y0.max(rect.y1);

        (x1 - x0 > f32::EPSILON && y1 - y0 > f32::EPSILON).then(|| Rect::new(x0, y0, x1, y1))
    }

    fn rects_intersect(a: Rect, b: Rect) -> bool {
        a.x0 < b.x1 && a.x1 > b.x0 && a.y0 < b.y1 && a.y1 > b.y0
    }

    fn distance_to_rect(point: mupdf::Point, rect: Rect) -> f32 {
        Self::squared_distance_to_rect(point, rect).sqrt()
    }

    fn squared_distance_to_rect(point: mupdf::Point, rect: Rect) -> f32 {
        let dx = if point.x < rect.x0 {
            rect.x0 - point.x
        } else if point.x > rect.x1 {
            point.x - rect.x1
        } else {
            0.0
        };
        let dy = if point.y < rect.y0 {
            rect.y0 - point.y
        } else if point.y > rect.y1 {
            point.y - rect.y1
        } else {
            0.0
        };

        dx * dx + dy * dy
    }

    fn with_alpha(rgb: u32, alpha: u8) -> u32 {
        ((rgb & 0x00ff_ffff) << 8) | alpha as u32
    }
}

impl EventEmitter<PdfViewEvent> for PdfView {}

impl Render for PdfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let prepaint_view = view.clone();
        let (theme, window_settings) = {
            let store = cx.global::<SettingsStore>();
            (store.resolved_theme, store.settings.window)
        };
        if self.window_settings != window_settings {
            self.transparency_enabled = window_settings.transparency_enabled_by_default();
            self.window_settings = window_settings;
            window.set_background_appearance(
                window_settings.background_appearance(self.transparency_enabled),
            );
        }
        let opacity = window_settings.effective_opacity(self.transparency_enabled);
        if self.loading || self.loading_error.is_some() {
            return self.render_loading_state(theme, opacity, cx);
        }

        if !self.diff_flashes.is_empty() {
            window.request_animation_frame();
        }
        if !self.is_presentation_mode() {
            if let Some(offset) = self.document_scroll.take_pending_offset() {
                let scale = self.effective_scale().max(0.001);
                let target = (-offset.y.as_f32() / scale).clamp(0.0, self.max_scroll_y_doc());
                if self.set_scroll_doc(self.scroll_x_doc, target, cx) {
                    self.mark_manual_navigation();
                }
            }
            self.step_scroll_animation(window, cx);
        }
        let context_menu_highlight_color = if self.grid_overview_open {
            None
        } else {
            self.context_menu_highlight
                .and_then(|id| self.highlight_color_for_id(id))
        };
        let context_menu_has_selection = !self.grid_overview_open && self.selected_text().is_some();
        let context_menu_has_note = !self.grid_overview_open && self.context_menu_note.is_some();
        let context_menu = self.context_menu_position.map(|position| {
            deferred(
                anchored()
                    .snap_to_window_with_margin(px(8.0))
                    .position(position)
                    .anchor(Anchor::TopLeft)
                    .child(Self::render_context_menu(
                        ContextMenuState {
                            has_page: self.capabilities.page_export
                                && !self.context_menu_region
                                && self.context_menu_page.is_some(),
                            has_image: !self.grid_overview_open
                                && self.context_menu_image.is_some(),
                            has_selection: context_menu_has_selection,
                            highlight_color: context_menu_highlight_color,
                            has_note: context_menu_has_note,
                            has_region: self.context_menu_region,
                            grid_overview: self.grid_overview_open,
                            copy_submenu_open: self.context_menu_copy_submenu_open,
                        },
                        window.is_fullscreen(),
                        theme,
                        cx,
                    )),
            )
            .with_priority(1)
        });
        let region_popover = (!self.grid_overview_open
            && self.context_menu_position.is_none()
            && self.region_drag.is_none())
        .then(|| self.region_popover_position())
        .flatten()
        .map(|position| {
            deferred(
                anchored()
                    .snap_to_window_with_margin(px(8.0))
                    .position(position)
                    .anchor(Anchor::TopRight)
                    .child(Self::render_region_popover(theme, cx)),
            )
            .with_priority(2)
        });
        let epub_style_panel = self.epub_style_panel_position.map(|position| {
            deferred(
                anchored()
                    .snap_to_window_with_margin(px(8.0))
                    .position(position)
                    .anchor(Anchor::BottomLeft)
                    .child(self.render_epub_style_panel(theme, cx)),
            )
            .with_priority(3)
        });
        let note_popup = if self.context_menu_position.is_none() && self.note_editor.is_none() {
            self.hovered_note
                .and_then(|id| self.hovered_note_position.zip(self.note_body_for_id(id)))
                .map(|(position, body)| {
                    deferred(
                        anchored()
                            .snap_to_window_with_margin(px(8.0))
                            .position(point(position.x + px(12.0), position.y + px(12.0)))
                            .anchor(Anchor::TopLeft)
                            .child(Self::render_note_popup(body, theme)),
                    )
                    .with_priority(1)
                })
        } else {
            None
        };
        let translation_popup = self.translation_popup.clone().and_then(|state| {
            state.position.map(|position| {
                deferred(
                    anchored()
                        .snap_to_window_with_margin(px(8.0))
                        .position(point(position.x + px(12.0), position.y + px(12.0)))
                        .anchor(Anchor::TopLeft)
                        .child(self.render_translation_card(state, false, theme, window, cx)),
                )
                .with_priority(2)
            })
        });
        let pinned_translation = self.pinned_translation.clone().map(|state| {
            let corner = cx
                .global::<SettingsStore>()
                .settings
                .translation
                .pinned
                .corner;
            let card = self.render_translation_card(state, true, theme, window, cx);
            let mut container = div().absolute().m(px(16.0));
            container = match corner {
                TranslationBoxCorner::TopLeft => container.top(px(0.0)).left(px(0.0)),
                TranslationBoxCorner::TopRight => container.top(px(0.0)).right(px(0.0)),
                TranslationBoxCorner::BottomLeft => container.bottom(px(0.0)).left(px(0.0)),
                TranslationBoxCorner::BottomRight => container.bottom(px(0.0)).right(px(0.0)),
            };
            container.child(card)
        });

        div()
            .size_full()
            .flex()
            .flex_col()
            .cursor_text()
            .when(self.region_tool_active, |this| this.cursor_crosshair())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_epub_style_panel(cx);
                }),
            )
            .when(
                !self.region_tool_active
                    && (self.hovered_link.is_some() || self.hovered_note.is_some()),
                |this| this.cursor_pointer(),
            )
            .when(self.panning.is_some(), |this| this.cursor_grabbing())
            .key_context(if self.region_tool_active {
                "PdfView RegionSelection"
            } else if self.grid_overview_open {
                "GridOverview"
            } else if self.is_presentation_mode() {
                "PdfView PresentationMode"
            } else {
                "PdfView"
            })
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Undo, _, cx| {
                this.undo_document_edit(cx);
            }))
            .on_action(cx.listener(|this, _: &Redo, _, cx| {
                this.redo_document_edit(cx);
            }))
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| {
                this.copy_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &TranslateSelection, _, cx| {
                this.close_context_menu(cx);
                this.translate_selection(
                    this.selection_bounds()
                        .and_then(|(start, end)| {
                            this.highlight_bounds_for_range(start..end + 1)
                                .last()
                                .map(|bounds| {
                                    point(
                                        bounds.right() + px(4.0),
                                        bounds.bottom() + px(4.0),
                                    )
                                })
                        }),
                    false,
                    cx,
                );
            }))
            .when(self.can_translate_document(), |this| {
                this.on_action(cx.listener(|this, _: &TranslateDocument, _, cx| {
                    this.translate_document(cx);
                }))
            })
            .when(
                self.document_translation_jobs
                    .read(cx)
                    .has_job_for_source(&self.path),
                |this| {
                this.on_action(
                    cx.listener(|this, _: &CancelDocumentTranslation, _, cx| {
                        this.cancel_document_translation(cx);
                    }),
                )
                },
            )
            .on_action(cx.listener(|this, _: &TogglePinnedTranslation, _, cx| {
                this.toggle_pinned_translation(cx);
            }))
            .on_action(cx.listener(|this, _: &OpenTranslationSplit, _, cx| {
                if this.capabilities.translation_split {
                    this.open_translation_split(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleRegionSelection, _, cx| {
                this.toggle_region_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &CancelRegionSelection, _, cx| {
                this.cancel_region_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.select_all();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                this.zoom_in(cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                this.zoom_out(cx);
            }))
            .on_action(cx.listener(|this, _: &ResetZoom, _, cx| {
                this.reset_zoom(cx);
            }))
            .on_action(cx.listener(|this, _: &RotateClockwise, _, cx| {
                this.rotate_clockwise(cx);
            }))
            .on_action(cx.listener(|this, _: &RotateCounterclockwise, _, cx| {
                this.rotate_counterclockwise(cx);
            }))
            .on_action(cx.listener(|this, _: &Reload, _, cx| {
                this.request_reload(cx);
            }))
            .on_action(cx.listener(|this, _: &Search, window, cx| {
                this.toggle_search(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenQuickAsk, window, cx| {
                this.toggle_quick_ask(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenGoToPage, window, cx| {
                this.toggle_go_to_page_prompt(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextSearchResult, _, cx| {
                this.navigate_search(false, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevSearchResult, _, cx| {
                this.navigate_search(true, cx);
            }))
            .on_action(cx.listener(|this, _: &NextPage, _, cx| {
                this.next_page(cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousPage, _, cx| {
                this.previous_page(cx);
            }))
            .on_action(cx.listener(|this, _: &NextHalfPage, _, cx| {
                this.next_half_page(cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousHalfPage, _, cx| {
                this.previous_half_page(cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollLineDown, _, cx| {
                this.scroll_line(1.0, cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollLineUp, _, cx| {
                this.scroll_line(-1.0, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToBeginning, _, cx| {
                this.go_to_beginning(cx);
            }))
            .on_action(cx.listener(|this, _: &GoToEnd, _, cx| {
                this.go_to_end(cx);
            }))
            .on_action(cx.listener(|this, _: &GridCursorUp, _, cx| {
                this.grid_cursor_up(cx);
            }))
            .on_action(cx.listener(|this, _: &GridCursorDown, _, cx| {
                this.grid_cursor_down(cx);
            }))
            .on_action(cx.listener(|this, _: &GridCursorLeft, _, cx| {
                this.grid_cursor_left(cx);
            }))
            .on_action(cx.listener(|this, _: &GridCursorRight, _, cx| {
                this.grid_cursor_right(cx);
            }))
            .on_action(cx.listener(|this, _: &JumpBackward, _, cx| {
                this.jump_backward(cx);
            }))
            .on_action(cx.listener(|this, _: &JumpForward, _, cx| {
                this.jump_forward(cx);
            }))
            .on_action(cx.listener(|this, _: &Toc, window, cx| {
                this.toggle_toc_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleToc, window, cx| {
                this.toggle_toc_sidebar(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenNotes, window, cx| {
                this.toggle_notes_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleNotes, window, cx| {
                this.toggle_notes_sidebar(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleThumbnailSidebar, window, cx| {
                this.toggle_thumbnail_sidebar(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleGridOverview, _, cx| {
                this.toggle_grid_overview(cx);
            }))
            .on_action(cx.listener(|this, _: &GridOverviewConfirm, _, cx| {
                this.confirm_grid_overview_selection(cx);
            }))
            .on_action(cx.listener(|_, _: &ToggleFullScreen, window, _| {
                window.toggle_fullscreen();
            }))
            .on_action(cx.listener(|this, _: &TogglePresentationMode, window, cx| {
                this.toggle_presentation_mode(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleTransparency, window, cx| {
                this.toggle_transparency(window, cx);
            }))
            .on_action(cx.listener(|_, _: &OpenSettings, _, cx| {
                cx.emit(PdfViewEvent::OpenSettings);
            }))
            .on_action(cx.listener(|this, _: &ToggleAiPanel, window, cx| {
                this.toggle_ai_panel(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenHistory, window, cx| {
                this.toggle_history_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenBookmarks, window, cx| {
                this.toggle_bookmark_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &AddBookmark, window, cx| {
                this.toggle_bookmark_prompt(window, cx);
            }))
            .when(self.capabilities.publisher_styles, |this| {
                this.on_action(cx.listener(|this, _: &TogglePublisherStyles, _, cx| {
                    this.toggle_epub_publisher_styles(cx);
                }))
            })
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                this.handle_scroll_wheel(event, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.close_context_menu(cx);
                    this.close_thumbnail_sidebar_actions(cx);
                    this.focus_handle.focus(window, cx);
                    this.dragging = false;
                    this.pending_link = None;
                    this.pending_selection_anchor = None;

                    if this.region_tool_active {
                        this.selection = None;
                        this.selected_highlight = None;
                        this.selected_region = None;
                        this.region_drag = this
                            .point_in_image(event.position)
                            .then(|| this.screen_to_document(event.position))
                            .flatten()
                            .map(|anchor| RegionDragState {
                                anchor,
                                current: anchor,
                            });
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }

                    if let Some(link_ix) = this.link_at_screen(event.position) {
                        this.selection = None;
                        this.selected_highlight = None;
                        this.pending_link = Some(link_ix);
                        this.set_hovered_link(Some(link_ix), cx);
                        cx.stop_propagation();
                        return;
                    }

                    this.set_hovered_link(None, cx);
                    this.set_hovered_note(None, None, cx);
                    if !this.point_in_image(event.position) {
                        this.selection = None;
                        this.selected_highlight = None;
                        cx.notify();
                        return;
                    }

                    if let Some(highlight_id) = this.highlight_at_screen(event.position) {
                        this.selection = None;
                        this.selected_highlight = Some(highlight_id);
                        cx.notify();
                        return;
                    }

                    if let Some(ix) = this.nearby_glyph_at_screen(event.position) {
                        this.selected_highlight = None;
                        if event.click_count == 2 {
                            if let Some(range) = this.word_bounds_at_glyph(ix) {
                                this.selection = Some(Selection {
                                    anchor: range.start,
                                    head: range.end - 1,
                                });
                            } else {
                                this.selection = Some(Selection {
                                    anchor: ix,
                                    head: ix,
                                });
                                this.dragging = true;
                            }
                        } else {
                            this.selection = None;
                            this.pending_selection_anchor = Some(ix);
                            this.dragging = true;
                        }
                    } else {
                        this.selection = None;
                        this.selected_highlight = None;
                    }

                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.close_context_menu(cx);
                    this.close_thumbnail_sidebar_actions(cx);
                    this.focus_handle.focus(window, cx);
                    this.dragging = false;
                    this.pending_link = None;
                    this.pending_selection_anchor = None;
                    this.set_hovered_link(None, cx);
                    this.set_hovered_note(None, None, cx);
                    this.panning = Some(PanState {
                        pointer_origin: event.position,
                        scroll_origin_x: this.scroll_x_doc,
                        scroll_origin_y: this.scroll_y_doc,
                    });
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if this.search_bar.is_some()
                        || this.quick_ask.is_some()
                        || this.toc_palette.is_some()
                        || this.history_palette.is_some()
                        || this.bookmark_palette.is_some()
                        || this.bookmark_prompt.is_some()
                        || this.go_to_page_prompt.is_some()
                        || this.note_editor.is_some()
                    {
                        window.prevent_default();
                        cx.stop_propagation();
                        return;
                    }

                    this.context_menu_highlight = this.highlight_at_screen(event.position);
                    this.context_menu_note = this.note_at_screen(event.position);
                    this.context_menu_page = this.page_at_screen(event.position);
                    if let Some(highlight_id) = this.context_menu_highlight {
                        this.selected_highlight = Some(highlight_id);
                        this.selection = None;
                    } else {
                        this.selected_highlight = None;
                    }
                    this.open_context_menu(event.position, window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if let Some(mut drag) = this.region_drag {
                    if !event.dragging() {
                        this.region_drag = None;
                        cx.notify();
                        return;
                    }
                    if let Some(current) = this.screen_to_document(event.position) {
                        drag.current = current;
                        this.region_drag = Some(drag);
                        cx.notify();
                    }
                    return;
                }

                if let Some(pan) = this.panning {
                    this.set_hovered_note(None, None, cx);
                    let scale = this.effective_scale();
                    let delta_x = (event.position.x - pan.pointer_origin.x).as_f32();
                    let delta_y = (event.position.y - pan.pointer_origin.y).as_f32();
                    if this.set_scroll_doc(
                        pan.scroll_origin_x - delta_x / scale,
                        pan.scroll_origin_y - delta_y / scale,
                        cx,
                    ) {
                        this.mark_manual_navigation();
                    }
                    return;
                }

                if !this.dragging {
                    this.update_hovered_link(event.position, cx);
                    this.update_hovered_note(event.position, cx);
                    this.set_hovered_text_block(
                        this.glyph_at_screen(event.position)
                            .and_then(|glyph_ix| this.glyphs.get(glyph_ix))
                            .map(|glyph| glyph.block_idx),
                        cx,
                    );
                    return;
                }

                if !event.dragging() {
                    this.dragging = false;
                    this.pending_selection_anchor = None;
                    this.update_hovered_link(event.position, cx);
                    this.update_hovered_note(event.position, cx);
                    this.set_hovered_text_block(None, cx);
                    return;
                }

                this.set_hovered_link(None, cx);
                this.set_hovered_note(None, None, cx);
                if let Some(ix) = this.glyph_at_screen(event.position) {
                    if let Some(selection) = this.selection.as_mut() {
                        selection.head = ix;
                    } else if let Some(anchor) = this.pending_selection_anchor {
                        this.selection = Some(Selection { anchor, head: ix });
                    }
                    this.emit_linked_selection(cx);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    if let Some(mut drag) = this.region_drag.take() {
                        if let Some(current) = this.screen_to_document(event.position) {
                            drag.current = current;
                        }
                        this.selected_region = this.region_reference_for_drag(drag);
                        this.region_tool_active = false;
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }

                    let pending_link = this.pending_link.take();
                    this.dragging = false;
                    this.pending_selection_anchor = None;
                    if let Some(link_ix) = pending_link {
                        if this.link_at_screen(event.position) == Some(link_ix) {
                            this.activate_link(link_ix, cx);
                            cx.stop_propagation();
                        }
                        cx.notify();
                    }
                    if this.pinned_translation.is_some() && this.selected_text().is_some()
                    {
                        this.translate_selection(None, true, cx);
                    }
                    this.emit_linked_selection(cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _, _, cx| {
                    this.panning = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .bg(rgb(theme.tab_active_background))
                    .when(opacity < 1.0, |this| this.opacity(opacity))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .when(!self.grid_overview_open, |this| {
                                this.child(
                                    canvas(
                                        move |bounds, window, cx| {
                                            prepaint_view.update(cx, |this, cx| {
                                                this.paint_data(
                                                    bounds,
                                                    window.scale_factor(),
                                                    window,
                                                    cx,
                                                )
                                                .expect("failed to render PDF")
                                            })
                                        },
                                        move |_, paint_data, window, cx| {
                                            for page in paint_data.pages {
                                                window
                                                    .paint_quad(fill(page.bounds, rgb(theme.page)));

                                                if let Some(image) = page.image {
                                                    let _ = window.paint_image(
                                                        page.bounds,
                                                        Corners::default(),
                                                        image,
                                                        0,
                                                        false,
                                                    );
                                                }
                                            }

                                            for overlay in paint_data.translated_text_overlays {
                                                window.paint_quad(fill(
                                                    overlay.mask,
                                                    rgb(theme.page),
                                                ));
                                                let mut y = overlay.bounds.top();
                                                for line in &overlay.prepared.lines {
                                                    if y >= overlay.bounds.bottom() {
                                                        break;
                                                    }
                                                    if let Err(error) = line.as_ref().paint(
                                                        point(overlay.bounds.left(), y),
                                                        overlay.prepared.line_height,
                                                        TextAlign::Left,
                                                        Some(overlay.bounds),
                                                        window,
                                                        cx,
                                                    ) {
                                                        eprintln!(
                                                            "failed to paint translated PDF text: {error:#}"
                                                        );
                                                        break;
                                                    }
                                                    y += line
                                                        .size(overlay.prepared.line_height)
                                                        .height;
                                                }
                                            }

                                            for overlay in paint_data.diff_flash_overlays {
                                                window.paint_quad(fill(
                                                    overlay.bounds,
                                                    rgba(Self::with_alpha(
                                                        theme.accent,
                                                        overlay.alpha,
                                                    )),
                                                ));
                                            }

                                            for (highlight, color) in paint_data.highlight_overlays
                                            {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    rgba(theme.highlight_color(color)),
                                                ));
                                            }

                                            for (note, color) in paint_data.note_underlines {
                                                let dot = px(3.0);
                                                let gap = px(3.0);
                                                let step = dot + gap;
                                                let y = note.origin.y + note.size.height
                                                    - dot
                                                    - px(1.0);
                                                let right = note.origin.x + note.size.width;
                                                let mut x = note.origin.x;
                                                while x < right {
                                                    let width = if x + dot > right {
                                                        right - x
                                                    } else {
                                                        dot
                                                    };
                                                    window.paint_quad(
                                                        fill(
                                                            Bounds::new(
                                                                point(x, y),
                                                                size(width, dot),
                                                            ),
                                                            rgba(theme.note_color(color)),
                                                        )
                                                        .corner_radii(Corners::all(dot / 2.0)),
                                                    );
                                                    x += step;
                                                }
                                            }

                                            for highlight in paint_data.search_highlights {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    rgba(Self::with_alpha(
                                                        theme.match_highlight,
                                                        0x66,
                                                    )),
                                                ));
                                            }

                                            for highlight in paint_data.active_search_highlights {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    rgba(Self::with_alpha(theme.accent, 0x99)),
                                                ));
                                            }

                                            for highlight in paint_data.link_highlights {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    rgba(Self::with_alpha(theme.accent, 0x33)),
                                                ));
                                            }

                                            for highlight in paint_data.linked_hover_highlights {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    rgba(Self::with_alpha(theme.accent, 0x44)),
                                                ));
                                            }

                                            for highlight in paint_data.linked_selection_highlights {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    rgba(Self::with_alpha(theme.accent, 0x66)),
                                                ));
                                            }

                                            for border in paint_data.selected_highlight_borders {
                                                let color =
                                                    rgba(Self::with_alpha(theme.accent, 0xdd));
                                                let thickness = px(1.0);
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        border.origin,
                                                        size(border.size.width, thickness),
                                                    ),
                                                    color,
                                                ));
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        point(
                                                            border.origin.x,
                                                            border.origin.y + border.size.height
                                                                - thickness,
                                                        ),
                                                        size(border.size.width, thickness),
                                                    ),
                                                    color,
                                                ));
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        border.origin,
                                                        size(thickness, border.size.height),
                                                    ),
                                                    color,
                                                ));
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        point(
                                                            border.origin.x + border.size.width
                                                                - thickness,
                                                            border.origin.y,
                                                        ),
                                                        size(thickness, border.size.height),
                                                    ),
                                                    color,
                                                ));
                                            }

                                            for highlight in paint_data.selection_highlights {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    rgba(theme.selection_highlight),
                                                ));
                                            }

                                            for region in paint_data.region_overlays {
                                                window.paint_quad(fill(
                                                    region,
                                                    rgba(Self::with_alpha(theme.accent, 0x33)),
                                                ));
                                                window.paint_quad(outline(
                                                    region,
                                                    rgba(Self::with_alpha(theme.accent, 0xee)),
                                                    BorderStyle::Solid,
                                                ));
                                            }
                                        },
                                    )
                                    .size_full(),
                                )
                            })
                            .when(
                                !self.is_presentation_mode() && !self.grid_overview_open,
                                |this| {
                                    this.vertical_scrollbar_for(&self.document_scroll, window, cx)
                                },
                            ),
                    ),
            )
            .when(self.grid_overview_open, |this| {
                this.child(self.render_grid_overview(theme, window, cx))
            })
            .when_some(self.search_bar.clone(), |this, search_bar| {
                this.child(
                    div()
                        .absolute()
                        .top(px(12.0))
                        .right(px(16.0))
                        .child(search_bar),
                )
            })
            .when(self.quick_ask_visible, |this| {
                this.when_some(self.quick_ask.clone(), |this, quick_ask| {
                    this.child(
                        div()
                            .absolute()
                            .left(px(0.0))
                            .right(px(0.0))
                            .bottom_0()
                            .flex()
                            .child(quick_ask),
                    )
                })
            })
            .when_some(self.toc_palette.clone(), |this, palette| {
                this.child(Self::render_centered_overlay(
                    palette,
                    |this, window, cx| this.close_toc_palette(window, cx),
                    theme,
                    cx,
                ))
            })
            .when_some(translation_popup, |this, popup| this.child(popup))
            .when_some(pinned_translation, |this, pinned| this.child(pinned))
            .when_some(self.notes_palette.clone(), |this, palette| {
                this.child(Self::render_centered_overlay(
                    palette,
                    |this, window, cx| this.close_notes_palette(window, cx),
                    theme,
                    cx,
                ))
            })
            .when_some(self.history_palette.clone(), |this, palette| {
                this.child(Self::render_centered_overlay(
                    palette,
                    |this, window, cx| this.close_history_palette(window, cx),
                    theme,
                    cx,
                ))
            })
            .when_some(self.bookmark_palette.clone(), |this, palette| {
                this.child(Self::render_centered_overlay(
                    palette,
                    |this, window, cx| this.close_bookmark_palette(window, cx),
                    theme,
                    cx,
                ))
            })
            .when_some(self.bookmark_prompt.clone(), |this, prompt| {
                this.child(Self::render_centered_overlay(
                    prompt,
                    |this, window, cx| this.close_bookmark_prompt(window, cx),
                    theme,
                    cx,
                ))
            })
            .when_some(self.go_to_page_prompt.clone(), |this, prompt| {
                this.child(Self::render_centered_overlay(
                    prompt,
                    |this, window, cx| this.close_go_to_page_prompt(window, cx),
                    theme,
                    cx,
                ))
            })
            .when_some(self.note_editor.clone(), |this, editor| {
                this.child(Self::render_centered_overlay(
                    editor,
                    |this, window, cx| this.close_note_editor(window, cx),
                    theme,
                    cx,
                ))
            })
            .children(note_popup)
            .children(context_menu)
            .children(region_popover)
            .children(epub_style_panel)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Arc};

    use super::{
        Glyph, PdfPage, PdfView, RegionDragState, RegionImageExport, RenderCache, RenderKey,
        SearchState, TextChunk, TextFormatBlock, TextIndexState, TextLine, TranslationCardState,
        TranslationStatus, clear_copied_region_selection,
        reset_translation_cards_for_document_replacement, retain_valid_annotation_id,
    };
    use glyphst_ai::ai_chat::{MentionKind, MentionReference};
    use glyphst_core::{
        DocumentLayout, DocumentRotation, EpubLayout, PageIndex, PageRegion, RegionImageReference,
    };
    use glyphst_translation::LanguageCode;
    use gpui::{Modifiers, ScrollWheelEvent, point, px};
    use image::{Frame, GenericImageView, RgbaImage};
    use mupdf::{Point, Quad, Rect};
    use smallvec::smallvec;

    #[test]
    fn annotation_reconciliation_clears_only_stale_ids() {
        let valid_ids = HashSet::from([7, 11]);
        let mut retained = Some(7);
        let mut removed = Some(9);
        let mut empty = None;

        assert!(!retain_valid_annotation_id(&mut retained, &valid_ids));
        assert_eq!(retained, Some(7));
        assert!(retain_valid_annotation_id(&mut removed, &valid_ids));
        assert_eq!(removed, None);
        assert!(!retain_valid_annotation_id(&mut empty, &valid_ids));
    }

    #[test]
    fn page_counter_uses_generated_pages_for_any_document_format() {
        assert_eq!(PdfView::page_counter_label(3, 12), "3 / 12");
    }

    #[test]
    fn completed_text_index_does_not_restart() {
        let mut state = TextIndexState::Pending;

        assert!(state.begin());
        assert_eq!(state, TextIndexState::Indexing);
        assert!(!state.begin());

        state.settle(true);
        assert_eq!(state, TextIndexState::Ready);
        assert!(!state.begin());
    }

    #[test]
    fn failed_text_index_does_not_retry_implicitly() {
        let mut state = TextIndexState::Pending;

        assert!(state.begin());
        state.settle(false);

        assert_eq!(state, TextIndexState::Failed);
        assert!(!state.begin());
    }

    #[test]
    fn search_reveal_after_indexing_is_consumed_once() {
        let mut search = SearchState {
            query: "needle".into(),
            ..Default::default()
        };

        search.update_reveal_after_indexing(true);

        assert!(search.take_reveal_after_indexing());
        assert!(!search.take_reveal_after_indexing());
    }

    #[test]
    fn manual_navigation_cancels_search_reveal_after_indexing() {
        let mut search = SearchState {
            query: "needle".into(),
            ..Default::default()
        };

        search.update_reveal_after_indexing(true);
        search.cancel_reveal_after_indexing();

        assert!(!search.take_reveal_after_indexing());
    }

    #[test]
    fn empty_search_does_not_request_reveal_after_indexing() {
        let mut search = SearchState::default();

        search.update_reveal_after_indexing(true);

        assert!(!search.take_reveal_after_indexing());
    }

    #[test]
    fn page_block_markdown_escapes_commonmark_and_preserves_spacing() {
        let blocks = vec![
            TextFormatBlock {
                lines: vec![
                    "# Heading".to_string(),
                    "literal *stars* and [link](url)".to_string(),
                    String::new(),
                ],
                average_font_size: 12.0,
                bold_ratio: 0.0,
                layout: None,
            },
            TextFormatBlock {
                lines: vec!["next_block.".to_string()],
                average_font_size: 12.0,
                bold_ratio: 0.0,
                layout: None,
            },
        ];

        assert_eq!(
            PdfView::format_block_markdown(&blocks),
            "\\# Heading\nliteral \\*stars\\* and \\[link\\]\\(url\\)\n\n\nnext\\_block\\."
        );
    }

    #[test]
    fn semantic_page_markdown_converts_conservative_headings_and_lists() {
        let blocks = vec![
            TextFormatBlock {
                lines: vec!["Overview".to_string()],
                average_font_size: 24.0,
                bold_ratio: 0.9,
                layout: None,
            },
            TextFormatBlock {
                lines: vec!["plain *text*.".to_string()],
                average_font_size: 12.0,
                bold_ratio: 0.0,
                layout: None,
            },
            TextFormatBlock {
                lines: vec!["• First *item*".to_string(), "- Second".to_string()],
                average_font_size: 12.0,
                bold_ratio: 0.0,
                layout: None,
            },
            TextFormatBlock {
                lines: vec!["1) One".to_string(), "2. Two".to_string()],
                average_font_size: 12.0,
                bold_ratio: 0.0,
                layout: None,
            },
        ];

        assert_eq!(
            PdfView::format_semantic_markdown(&blocks),
            "# Overview\n\nplain \\*text\\*\\.\n\n- First \\*item\\*\n- Second\n\n1. One\n2. Two"
        );
    }

    #[test]
    fn selected_markdown_uses_glyph_boundaries_and_typography() {
        let mut glyphs = "Overview"
            .chars()
            .map(|c| test_glyph(c, 0.0, 6.0))
            .collect::<Vec<_>>();
        glyphs.extend("• First".chars().map(|c| {
            let mut glyph = test_glyph(c, 0.0, 6.0);
            glyph.block_idx = 1;
            glyph.line_idx = 1;
            glyph
        }));
        for glyph in &mut glyphs[..8] {
            glyph.font_size = 24.0;
            glyph.is_bold = true;
        }

        let blocks = PdfView::text_blocks_for_glyph_range(&glyphs, 0..glyphs.len()).unwrap();

        assert_eq!(
            PdfView::format_semantic_markdown(&blocks),
            "# Overview\n\n- First"
        );
    }

    #[test]
    fn semantic_copy_joins_wrapped_lines_for_plain_text_and_markdown() {
        let mut glyphs = Vec::new();
        for (line_idx, text) in ["Two lines in a PDF document", "can be copied as one line."]
            .into_iter()
            .enumerate()
        {
            glyphs.extend(text.chars().map(|c| {
                let mut glyph = test_glyph(c, 0.0, 6.0);
                glyph.line_idx = line_idx as u32;
                glyph
            }));
        }
        let semantic =
            PdfView::copy_blocks_for_glyph_range(&glyphs, 0..glyphs.len(), true).unwrap();
        let physical =
            PdfView::copy_blocks_for_glyph_range(&glyphs, 0..glyphs.len(), false).unwrap();

        assert_eq!(
            PdfView::plain_text_for_blocks(&semantic).as_deref(),
            Some("Two lines in a PDF document can be copied as one line.")
        );
        assert_eq!(
            PdfView::format_semantic_markdown(&semantic),
            "Two lines in a PDF document can be copied as one line\\."
        );
        assert_eq!(
            PdfView::plain_text_for_blocks(&physical).as_deref(),
            Some("Two lines in a PDF document\ncan be copied as one line.")
        );
        assert_eq!(
            PdfView::format_semantic_markdown(&physical),
            "Two lines in a PDF document\ncan be copied as one line\\."
        );
    }

    #[test]
    fn semantic_copy_joins_continuous_pdf_text_blocks() {
        let mut glyphs = Vec::new();
        for (block_idx, line_idx, text, line_rect) in [
            (
                0,
                0,
                "▪Message passing",
                Rect::new(442.0, 149.0, 632.0, 172.0),
            ),
            (1, 1, "between nodes", Rect::new(464.0, 175.0, 616.0, 193.0)),
        ] {
            glyphs.extend(text.chars().map(|c| {
                let mut glyph = test_glyph(c, line_rect.x0, line_rect.x1);
                glyph.block_idx = block_idx;
                glyph.line_idx = line_idx;
                glyph.line_rect = line_rect;
                glyph
            }));
        }

        let blocks = PdfView::copy_blocks_for_glyph_range(&glyphs, 0..glyphs.len(), true).unwrap();

        assert_eq!(
            PdfView::plain_text_for_blocks(&blocks).as_deref(),
            Some("▪Message passing between nodes")
        );
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn semantic_copy_keeps_parallel_columns_separate() {
        let mut glyphs = Vec::new();
        for (block_idx, text, line_rect) in [
            (0, "Shared memory", Rect::new(35.0, 149.0, 350.0, 172.0)),
            (
                1,
                "Distributed memory",
                Rect::new(442.0, 149.0, 632.0, 172.0),
            ),
        ] {
            glyphs.extend(text.chars().map(|c| {
                let mut glyph = test_glyph(c, line_rect.x0, line_rect.x1);
                glyph.block_idx = block_idx;
                glyph.line_idx = block_idx;
                glyph.line_rect = line_rect;
                glyph
            }));
        }

        let blocks = PdfView::copy_blocks_for_glyph_range(&glyphs, 0..glyphs.len(), true).unwrap();

        assert_eq!(
            PdfView::plain_text_for_blocks(&blocks).as_deref(),
            Some("Shared memory\n\nDistributed memory")
        );
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn semantic_copy_dehyphenates_word_continuations_and_preserves_list_items() {
        assert_eq!(
            PdfView::semantic_logical_lines(&[
                "context-aware dehyphen-".to_string(),
                "ation".to_string(),
                "• First item".to_string(),
                "continuation".to_string(),
                "• Second item".to_string(),
            ]),
            vec![
                "context-aware dehyphenation",
                "• First item continuation",
                "• Second item",
            ]
        );
        assert_eq!(
            PdfView::semantic_logical_lines(&["-".to_string(), "value".to_string()]),
            vec!["- value"]
        );
    }

    #[test]
    fn semantic_copy_preserves_text_block_boundaries() {
        let blocks = vec![
            TextFormatBlock {
                lines: vec!["left block".to_string()],
                average_font_size: 12.0,
                bold_ratio: 0.0,
                layout: None,
            },
            TextFormatBlock {
                lines: vec!["right block".to_string()],
                average_font_size: 12.0,
                bold_ratio: 0.0,
                layout: None,
            },
        ];

        assert_eq!(
            PdfView::plain_text_for_blocks(&blocks).as_deref(),
            Some("left block\n\nright block")
        );
    }

    #[test]
    fn plain_text_markdown_escapes_translated_text() {
        assert_eq!(
            PdfView::markdown_for_plain_text("literal *text*\n\nnext"),
            "literal \\*text\\*\n\nnext"
        );
    }

    #[test]
    fn page_image_dimensions_preserve_aspect_ratio_at_preset_edge() {
        assert_eq!(
            PdfView::page_image_dimensions(612.0, 792.0, 1600).unwrap(),
            (1236, 1600)
        );
        assert_eq!(
            PdfView::page_image_dimensions(792.0, 612.0, 1024).unwrap(),
            (1024, 791)
        );
        assert!(PdfView::page_image_dimensions(0.0, 612.0, 1024).is_err());
    }

    #[test]
    fn render_page_jpeg_encodes_rgb_pixmap() {
        let page_bounds = Rect::new(0.0, 0.0, 10.0, 20.0);
        let display_list = mupdf::DisplayList::new(page_bounds).unwrap();

        let bytes = PdfView::render_page_jpeg(
            &display_list,
            page_bounds,
            page_bounds.width(),
            page_bounds.height(),
            crate::settings::PageImageQuality::Low,
            DocumentRotation::Degrees0,
        )
        .unwrap();
        let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).unwrap();

        assert_eq!(image.dimensions(), (512, 1024));
    }

    #[test]
    fn rotated_page_jpeg_swaps_image_dimensions() {
        let page_bounds = Rect::new(0.0, 0.0, 10.0, 20.0);
        let display_list = mupdf::DisplayList::new(page_bounds).unwrap();

        let bytes = PdfView::render_page_jpeg(
            &display_list,
            page_bounds,
            page_bounds.width(),
            page_bounds.height(),
            crate::settings::PageImageQuality::Low,
            DocumentRotation::Degrees90,
        )
        .unwrap();
        let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).unwrap();

        assert_eq!(image.dimensions(), (1024, 512));
    }

    #[test]
    fn rotation_point_and_rect_transforms_round_trip() {
        let page_bounds = Rect::new(10.0, 20.0, 110.0, 220.0);
        let point = mupdf::Point { x: 35.0, y: 70.0 };
        let rect = Rect::new(20.0, 40.0, 60.0, 100.0);

        for rotation in [
            DocumentRotation::Degrees0,
            DocumentRotation::Degrees90,
            DocumentRotation::Degrees180,
            DocumentRotation::Degrees270,
        ] {
            let view_point = PdfView::pdf_point_to_view(page_bounds, rotation, point);
            let restored = PdfView::view_point_to_pdf(page_bounds, rotation, view_point);
            assert!((restored.x - point.x).abs() < 0.001);
            assert!((restored.y - point.y).abs() < 0.001);

            let view_rect = PdfView::pdf_rect_to_view(page_bounds, rotation, rect);
            let restored = PdfView::view_rect_to_pdf(page_bounds, rotation, view_rect);
            assert!((restored.x0 - rect.x0).abs() < 0.001);
            assert!((restored.y0 - rect.y0).abs() < 0.001);
            assert!((restored.x1 - rect.x1).abs() < 0.001);
            assert!((restored.y1 - rect.y1).abs() < 0.001);
        }
    }

    #[test]
    fn rotation_render_matrix_matches_view_transform() {
        let page_bounds = Rect::new(10.0, 20.0, 110.0, 220.0);
        let point = mupdf::Point { x: 35.0, y: 70.0 };
        let view_origin = mupdf::Point { x: 5.0, y: 7.0 };

        for rotation in [
            DocumentRotation::Degrees0,
            DocumentRotation::Degrees90,
            DocumentRotation::Degrees180,
            DocumentRotation::Degrees270,
        ] {
            let matrix = PdfView::render_matrix(page_bounds, rotation, 2.0, 3.0, view_origin);
            let transformed = matrix.transform_xy(point.x, point.y);
            let view_point = PdfView::pdf_point_to_view(page_bounds, rotation, point);
            assert!((transformed.0 - (view_point.x - view_origin.x) * 2.0).abs() < 0.001);
            assert!((transformed.1 - (view_point.y - view_origin.y) * 3.0).abs() < 0.001);
        }
    }

    #[test]
    fn render_cache_protects_only_the_visible_rotation() {
        let unrotated = RenderKey {
            doc_id: 1,
            page_idx: 0,
            rotation: DocumentRotation::Degrees0,
            physical_width: 8192,
            physical_height: 8192,
        };
        let rotated = RenderKey {
            rotation: DocumentRotation::Degrees90,
            ..unrotated
        };
        let image = Arc::new(gpui::RenderImage::new(smallvec![Frame::new(
            RgbaImage::new(1, 1),
        )]));
        let mut cache = RenderCache::new();
        cache.set_protected_pages([(1, 0, DocumentRotation::Degrees90)]);

        cache.insert_render(unrotated, 1.0, image.clone());
        cache.insert_render(rotated, 1.0, image);

        assert!(cache.render(unrotated).is_none());
        assert!(cache.render(rotated).is_some());
    }

    #[test]
    fn region_drag_intersects_centered_pages_and_omits_gaps() {
        let pages = vec![
            PdfPage {
                bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
                width: 100.0,
                height: 100.0,
                offset_y: 0.0,
                glyph_range: 0..0,
            },
            PdfPage {
                bounds: Rect::new(10.0, 20.0, 70.0, 120.0),
                width: 60.0,
                height: 100.0,
                offset_y: 120.0,
                glyph_range: 0..0,
            },
        ];
        let reference = PdfView::region_reference_for_pages(
            &pages,
            100.0,
            1.0,
            DocumentRotation::Degrees0,
            RegionDragState {
                anchor: Point::new(10.0, 50.0),
                current: Point::new(90.0, 170.0),
            },
        )
        .unwrap();

        assert_eq!(
            reference.regions,
            vec![
                PageRegion {
                    page: PageIndex(0),
                    x0: 10.0,
                    y0: 50.0,
                    x1: 90.0,
                    y1: 100.0,
                },
                PageRegion {
                    page: PageIndex(1),
                    x0: 10.0,
                    y0: 20.0,
                    x1: 70.0,
                    y1: 70.0,
                },
            ]
        );
    }

    #[test]
    fn region_drag_rejects_tiny_marquees() {
        let pages = [PdfPage {
            bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
            width: 100.0,
            height: 100.0,
            offset_y: 0.0,
            glyph_range: 0..0,
        }];
        assert!(
            PdfView::region_reference_for_pages(
                &pages,
                100.0,
                1.0,
                DocumentRotation::Degrees0,
                RegionDragState {
                    anchor: Point::new(10.0, 10.0),
                    current: Point::new(12.0, 30.0),
                },
            )
            .is_none()
        );
    }

    #[test]
    fn rotated_region_drag_maps_back_to_pdf_coordinates() {
        let pages = [PdfPage {
            bounds: Rect::new(10.0, 20.0, 110.0, 220.0),
            width: 100.0,
            height: 200.0,
            offset_y: 0.0,
            glyph_range: 0..0,
        }];
        let reference = PdfView::region_reference_for_pages(
            &pages,
            200.0,
            1.0,
            DocumentRotation::Degrees90,
            RegionDragState {
                anchor: Point::new(20.0, 10.0),
                current: Point::new(80.0, 50.0),
            },
        )
        .unwrap();

        assert_eq!(
            reference.regions,
            vec![PageRegion {
                page: PageIndex(0),
                x0: 20.0,
                y0: 140.0,
                x1: 60.0,
                y1: 200.0,
            }]
        );
    }

    #[test]
    fn successful_region_copy_clears_the_copied_selection() {
        let copied = RegionImageReference::new([PageRegion {
            page: PageIndex(0),
            x0: 10.0,
            y0: 20.0,
            x1: 30.0,
            y1: 40.0,
        }]);
        let mut selected_region = Some(copied.clone());
        let mut context_menu_region = true;

        assert!(clear_copied_region_selection(
            &mut selected_region,
            &mut context_menu_region,
            &copied,
        ));
        assert!(selected_region.is_none());
        assert!(!context_menu_region);
    }

    #[test]
    fn completed_region_copy_preserves_a_newer_selection() {
        let copied = RegionImageReference::new([PageRegion {
            page: PageIndex(0),
            x0: 10.0,
            y0: 20.0,
            x1: 30.0,
            y1: 40.0,
        }]);
        let newer = RegionImageReference::new([PageRegion {
            page: PageIndex(1),
            x0: 50.0,
            y0: 60.0,
            x1: 70.0,
            y1: 80.0,
        }]);
        let mut selected_region = Some(newer.clone());
        let mut context_menu_region = true;

        assert!(!clear_copied_region_selection(
            &mut selected_region,
            &mut context_menu_region,
            &copied,
        ));
        assert_eq!(selected_region, Some(newer));
        assert!(context_menu_region);
    }

    #[test]
    fn region_compositor_stacks_crops_and_centers_narrow_pages() {
        let page_bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
        let display_list = Arc::new(mupdf::DisplayList::new(page_bounds).unwrap());
        let exports = vec![
            RegionImageExport {
                display_list: display_list.clone(),
                page_bounds,
                region: PageRegion {
                    page: PageIndex(0),
                    x0: 0.0,
                    y0: 0.0,
                    x1: 10.0,
                    y1: 20.0,
                },
                rotation: DocumentRotation::Degrees0,
            },
            RegionImageExport {
                display_list,
                page_bounds,
                region: PageRegion {
                    page: PageIndex(1),
                    x0: 0.0,
                    y0: 0.0,
                    x1: 20.0,
                    y1: 10.0,
                },
                rotation: DocumentRotation::Degrees0,
            },
        ];

        let (width, height, rgba) = PdfView::rasterize_region_stack_to_rgba(&exports, 2.0).unwrap();
        assert_eq!((width, height), (40, 60));
        assert_eq!(rgba.len(), 40 * 60 * 4);
    }

    #[test]
    fn region_compositor_rejects_oversized_exports_before_rendering() {
        let page_bounds = Rect::new(0.0, 0.0, 10_000.0, 5_000.0);
        let display_list = Arc::new(mupdf::DisplayList::new(page_bounds).unwrap());
        let exports = [RegionImageExport {
            display_list,
            page_bounds,
            region: PageRegion {
                page: PageIndex(0),
                x0: 0.0,
                y0: 0.0,
                x1: 10_000.0,
                y1: 5_000.0,
            },
            rotation: DocumentRotation::Degrees0,
        }];

        assert!(PdfView::rasterize_region_stack_to_rgba(&exports, 1.0).is_err());
    }

    #[test]
    fn rotated_region_compositor_swaps_crop_dimensions() {
        let page_bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
        let exports = [RegionImageExport {
            display_list: Arc::new(mupdf::DisplayList::new(page_bounds).unwrap()),
            page_bounds,
            region: PageRegion {
                page: PageIndex(0),
                x0: 0.0,
                y0: 0.0,
                x1: 10.0,
                y1: 20.0,
            },
            rotation: DocumentRotation::Degrees90,
        }];

        let (width, height, _) = PdfView::rasterize_region_stack_to_rgba(&exports, 2.0).unwrap();
        assert_eq!((width, height), (40, 20));
    }

    #[test]
    fn page_reference_validation_rejects_invalid_pages_and_deduplicates_exact_refs() {
        let page = MentionReference::new(MentionKind::Page, Some(2));
        let image = MentionReference::new(MentionKind::PageImage, Some(2));
        let document_image = MentionReference::new(MentionKind::DocumentImage, None);
        let references = PdfView::prepare_page_references(
            vec![
                MentionReference::new(MentionKind::Selection, None),
                page,
                image,
                document_image,
                page,
                document_image,
            ],
            DocumentLayout::Fixed,
            3,
        )
        .unwrap();
        assert_eq!(
            references,
            vec![
                page,
                image,
                MentionReference::new(MentionKind::DocumentImage, Some(1)),
                MentionReference::new(MentionKind::DocumentImage, Some(2)),
                MentionReference::new(MentionKind::DocumentImage, Some(3)),
            ]
        );

        assert!(
            PdfView::prepare_page_references(
                vec![MentionReference::new(MentionKind::Page, None)],
                DocumentLayout::Fixed,
                3,
            )
            .unwrap_err()
            .to_string()
            .contains("requires a 1-based page number")
        );
        for invalid_page in [0, 4] {
            assert!(
                PdfView::prepare_page_references(
                    vec![MentionReference::new(
                        MentionKind::PageImage,
                        Some(invalid_page),
                    )],
                    DocumentLayout::Fixed,
                    3,
                )
                .unwrap_err()
                .to_string()
                .contains("page must be between 1 and 3")
            );
        }
        let paged_document_image = MentionReference::new(MentionKind::DocumentImage, Some(1));
        assert!(
            PdfView::prepare_page_references(vec![paged_document_image], DocumentLayout::Fixed, 3,)
                .unwrap_err()
                .to_string()
                .contains("does not accept a page number")
        );
        assert!(
            PdfView::prepare_page_references(
                vec![document_image],
                DocumentLayout::Reflow(EpubLayout::default()),
                3,
            )
            .unwrap_err()
            .to_string()
            .contains("reflowable documents")
        );
    }

    #[test]
    fn provisional_reflow_pages_use_uniform_sample_bounds() {
        let loaded =
            PdfView::provisional_reflow_pages(3, Rect::new(0.0, 0.0, 320.0, 480.0), 12.0).unwrap();
        assert_eq!(loaded.pages.len(), 3);
        assert_eq!(loaded.pages[0].offset_y, 0.0);
        assert_eq!(loaded.pages[1].offset_y, 492.0);
        assert_eq!(loaded.pages[2].offset_y, 984.0);
        assert_eq!(loaded.content_height_doc, 1464.0);
    }

    fn test_glyph(c: char, x0: f32, x1: f32) -> Glyph {
        Glyph {
            c,
            quad: Quad::new(
                Point::new(x0, 0.0),
                Point::new(x1, 0.0),
                Point::new(x0, 6.0),
                Point::new(x1, 6.0),
            ),
            line_rect: Rect::new(x0, 0.0, x1, 6.0),
            page_idx: 0,
            line_idx: 0,
            block_idx: 0,
            font_size: 12.0,
            color: 0,
            is_bold: false,
            inferred_space_after: false,
        }
    }

    #[test]
    fn wheel_zoom_uses_current_event_modifiers() {
        let plain = ScrollWheelEvent::default();
        assert!(!PdfView::scroll_wheel_requests_zoom(&plain));

        let zoom = ScrollWheelEvent {
            modifiers: Modifiers {
                control: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(PdfView::scroll_wheel_requests_zoom(&zoom));
    }

    #[test]
    fn document_replacement_clears_popup_and_preserves_pinned_mode() {
        let mut popup = Some(TranslationCardState::new(
            "popup",
            TranslationStatus::Loading,
            Some(point(px(10.0), px(20.0))),
            7,
        ));
        let mut pinned = Some(TranslationCardState::new(
            "pinned",
            TranslationStatus::Loading,
            None,
            7,
        ));

        reset_translation_cards_for_document_replacement(&mut popup, &mut pinned, 8);

        assert!(popup.is_none());
        let pinned = pinned.unwrap();
        assert!(pinned.source.is_empty());
        assert!(matches!(pinned.status, TranslationStatus::Idle));
        assert!(pinned.position.is_none());
        assert_eq!(pinned.request_generation, 8);
    }

    #[test]
    fn document_replacement_does_not_enable_pinned_mode() {
        let mut popup = None;
        let mut pinned = None;

        reset_translation_cards_for_document_replacement(&mut popup, &mut pinned, 8);

        assert!(popup.is_none());
        assert!(pinned.is_none());
    }

    #[test]
    fn document_translation_default_output_uses_target_language() {
        let target = LanguageCode::new("ZH-Hans").unwrap();

        assert_eq!(
            PdfView::document_translation_output_path(
                std::path::Path::new("/tmp/report.v1.pdf"),
                &target,
            ),
            std::path::PathBuf::from("/tmp/report.v1.zh-hans.pdf")
        );
    }

    fn missing_space_glyphs() -> Vec<Glyph> {
        let mut glyphs = [
            ('1', 0.0, 5.0),
            ('0', 5.0, 10.0),
            ('0', 10.0, 15.0),
            ('b', 16.6, 21.6),
            ('y', 21.6, 26.6),
            ('d', 28.2, 33.2),
            ('e', 33.2, 38.2),
            ('f', 38.2, 43.2),
            ('a', 43.2, 48.2),
            ('u', 48.2, 53.2),
            ('l', 53.2, 58.2),
            ('t', 58.2, 63.2),
        ]
        .into_iter()
        .map(|(c, x0, x1)| test_glyph(c, x0, x1))
        .collect::<Vec<_>>();
        PdfView::mark_inferred_spaces(&mut glyphs);
        glyphs
    }

    #[test]
    fn glyph_line_rect_uses_line_bounds_for_height() {
        let mut glyph = test_glyph('x', 2.0, 7.0);
        glyph.quad = Quad::new(
            Point::new(2.0, 2.0),
            Point::new(7.0, 2.0),
            Point::new(2.0, 4.0),
            Point::new(7.0, 4.0),
        );
        glyph.line_rect = Rect::new(0.0, 0.0, 10.0, 8.0);

        assert_eq!(
            PdfView::glyph_line_rect(&glyph),
            Rect::new(2.0, 0.0, 7.0, 8.0)
        );
    }

    #[test]
    fn nearest_line_uses_horizontal_distance_for_multi_column_pages() {
        let lines = vec![
            TextLine {
                glyph_range: 0..10,
                bounds: Rect::new(35.0, 148.0, 350.0, 172.0),
            },
            TextLine {
                glyph_range: 10..20,
                bounds: Rect::new(442.0, 149.0, 632.0, 172.0),
            },
        ];

        let right_text = PdfView::nearest_line_in_slice(&lines, Point::new(600.0, 180.0)).unwrap();
        let right_gutter =
            PdfView::nearest_line_in_slice(&lines, Point::new(410.0, 180.0)).unwrap();

        assert_eq!(right_text.1, 10);
        assert_eq!(right_gutter.1, 10);
    }

    #[test]
    fn nearest_line_is_stable_for_reverse_multi_column_drags() {
        let lines = vec![
            TextLine {
                glyph_range: 0..10,
                bounds: Rect::new(35.0, 148.0, 350.0, 172.0),
            },
            TextLine {
                glyph_range: 10..20,
                bounds: Rect::new(442.0, 149.0, 632.0, 172.0),
            },
            TextLine {
                glyph_range: 20..30,
                bounds: Rect::new(464.0, 175.0, 616.0, 193.0),
            },
        ];

        let lower = PdfView::nearest_line_in_slice(&lines, Point::new(610.0, 198.0)).unwrap();
        let upper = PdfView::nearest_line_in_slice(&lines, Point::new(620.0, 145.0)).unwrap();

        assert_eq!((lower.1, upper.1), (20, 10));
        assert_eq!((lower.1.min(upper.1), lower.2.max(upper.2)), (10, 29));
    }

    #[test]
    fn url_start_accepts_label_and_punctuation_boundaries() {
        for text in [
            "代码仓库：https://github.com/torvalds/linux.git",
            "(https://example.com/a/b)",
        ] {
            let chars = text.chars().collect::<Vec<_>>();
            let ix = chars.iter().position(|c| *c == 'h').unwrap();
            assert!(PdfView::url_start_boundary(&chars, ix));
        }
    }

    #[test]
    fn url_start_rejects_embedded_prefixes() {
        let chars = "abchttps://example.com/x".chars().collect::<Vec<_>>();
        let ix = chars.iter().position(|c| *c == 'h').unwrap();
        assert!(!PdfView::url_start_boundary(&chars, ix));
    }

    #[test]
    fn word_bounds_selects_words_between_punctuation() {
        let chars = "hello, world".chars().collect::<Vec<_>>();

        assert_eq!(PdfView::word_bounds_in_chars(&chars, 1), Some(0..5));
        assert_eq!(PdfView::word_bounds_in_chars(&chars, 8), Some(7..12));
        assert_eq!(PdfView::word_bounds_in_chars(&chars, 5), None);
        assert_eq!(PdfView::word_bounds_in_chars(&chars, 6), None);
    }

    #[test]
    fn word_bounds_includes_apostrophes_underscores_and_unicode() {
        let chars = "can't rust_lang café".chars().collect::<Vec<_>>();
        let apostrophe = chars.iter().position(|c| *c == '\'').unwrap();
        let underscore = chars.iter().position(|c| *c == '_').unwrap();
        let accented = chars.iter().position(|c| *c == 'é').unwrap();

        assert_eq!(
            PdfView::word_bounds_in_chars(&chars, apostrophe),
            Some(0..5)
        );
        assert_eq!(
            PdfView::word_bounds_in_chars(&chars, underscore),
            Some(6..15)
        );
        assert_eq!(
            PdfView::word_bounds_in_chars(&chars, accented),
            Some(16..20)
        );
    }

    #[test]
    fn word_bounds_stops_at_hyphens_and_empty_input() {
        let chars = "foo-bar".chars().collect::<Vec<_>>();

        assert_eq!(PdfView::word_bounds_in_chars(&chars, 1), Some(0..3));
        assert_eq!(PdfView::word_bounds_in_chars(&chars, 5), Some(4..7));
        assert_eq!(PdfView::word_bounds_in_chars(&chars, 3), None);
        assert_eq!(PdfView::word_bounds_in_chars(&[], 0), None);
    }

    #[test]
    fn word_bounds_stop_at_inferred_visual_spaces() {
        let glyphs = missing_space_glyphs();

        assert_eq!(PdfView::word_bounds_in_glyphs(&glyphs, 0), Some(0..3));
        assert_eq!(PdfView::word_bounds_in_glyphs(&glyphs, 3), Some(3..5));
        assert_eq!(PdfView::word_bounds_in_glyphs(&glyphs, 8), Some(5..12));
    }

    #[test]
    fn selected_text_inserts_inferred_visual_spaces() {
        let glyphs = missing_space_glyphs();

        assert_eq!(
            PdfView::text_for_glyph_range(&glyphs, 0..glyphs.len()).as_deref(),
            Some("100 by default")
        );
    }

    #[test]
    fn semantic_anchor_survives_shifted_glyph_offsets() {
        let original = "before selected passage after"
            .chars()
            .enumerate()
            .map(|(ix, c)| test_glyph(c, ix as f32, ix as f32 + 1.0))
            .collect::<Vec<_>>();
        let selected = "selected passage";
        let start = "before ".chars().count();
        let anchor = PdfView::semantic_anchor_for_glyph(&original, start);

        let mut shifted = "preface "
            .chars()
            .enumerate()
            .map(|(ix, c)| test_glyph(c, ix as f32, ix as f32 + 1.0))
            .collect::<Vec<_>>();
        shifted.extend(original);
        let resolved = PdfView::resolve_semantic_anchor(&shifted, &anchor).unwrap();
        let resolved_text = shifted[resolved..resolved + selected.chars().count()]
            .iter()
            .map(|glyph| glyph.c)
            .collect::<String>();

        assert_eq!(resolved_text, selected);
    }

    #[test]
    fn build_text_blocks_handles_large_glyph_sets_without_rescanning() {
        let mut glyphs = (0..100_000)
            .map(|index| {
                let mut glyph = test_glyph('a', index as f32 * 5.0, index as f32 * 5.0 + 5.0);
                glyph.block_idx = (index / 1000) as u32;
                glyph.line_idx = (index / 100) as u32;
                glyph
            })
            .collect::<Vec<_>>();
        for line in glyphs.chunks_mut(100) {
            PdfView::mark_inferred_spaces(line);
        }

        let blocks = PdfView::build_text_blocks(&glyphs);

        assert_eq!(blocks.len(), 100);
        assert!(blocks.iter().all(|block| block.text.len() >= 1000));
    }

    #[test]
    fn merge_text_chunks_offsets_block_and_page_indexes() {
        let first_glyphs = vec![test_glyph('a', 0.0, 5.0)];
        let second_glyphs = vec![test_glyph('b', 0.0, 5.0)];
        let first_bounds = PdfView::glyph_line_rect(&first_glyphs[0]);
        let second_bounds = PdfView::glyph_line_rect(&second_glyphs[0]);
        let first = TextChunk {
            start_page_idx: 0,
            text_blocks: PdfView::build_text_blocks(&first_glyphs),
            glyphs: first_glyphs,
            text_lines: vec![TextLine {
                glyph_range: 0..1,
                bounds: first_bounds,
            }],
            links: Vec::new(),
            page_glyph_ranges: std::iter::once(0..1).collect(),
            page_line_ranges: std::iter::once(0..1).collect(),
            page_block_ranges: std::iter::once(0..1).collect(),
            line_count: 1,
            block_count: 1,
        };
        let second = TextChunk {
            start_page_idx: 1,
            text_blocks: PdfView::build_text_blocks(&second_glyphs),
            glyphs: second_glyphs,
            text_lines: vec![TextLine {
                glyph_range: 0..1,
                bounds: second_bounds,
            }],
            links: Vec::new(),
            page_glyph_ranges: std::iter::once(0..1).collect(),
            page_line_ranges: std::iter::once(0..1).collect(),
            page_block_ranges: std::iter::once(0..1).collect(),
            line_count: 1,
            block_count: 1,
        };

        let merged = PdfView::merge_text_chunks(vec![second, first], 2).unwrap();

        assert_eq!(merged.page_glyph_ranges, vec![0..1, 1..2]);
        assert_eq!(merged.page_line_ranges, vec![0..1, 1..2]);
        assert_eq!(merged.page_block_ranges, vec![0..1, 1..2]);
        assert_eq!(merged.text_blocks[0].id, 0);
        assert_eq!(merged.text_blocks[1].id, 1);
        assert_eq!(merged.text_blocks[1].glyph_range, 1..2);
    }
}
