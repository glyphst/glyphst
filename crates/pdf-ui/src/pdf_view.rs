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
use crate::search::{SearchBar, SearchBarSnapshot, SearchEvent};
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
use glyphst_pdf::{
    DocumentRenderSession, DocumentSnapshot, ReloadCoordinator, ReloadDisposition,
    RetainedDisplayList,
};
use glyphst_pdf_translation_app::{PdfTranslationRequest, PdfTranslationService};
use glyphst_translation::{LanguageCode, TranslationRequest, TranslationService};
use glyphst_translation_ui::{
    DocumentTranslationJobManager, DocumentTranslationOpenMode, TargetLanguageOption,
    target_language_options,
};
use glyphst_ui::{
    activity::ActivityCenter,
    icons::toolbar_icon_button,
    markdown::{Markdown, MarkdownElement, MarkdownStyle},
    tooltip::text_tooltip,
};
use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, AsyncApp, BorderStyle, Bounds, ClipboardItem,
    Context, Corners, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, Font,
    FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point as GpuiPoint, Render, RenderImage, ScrollDelta, ScrollHandle, ScrollStrategy,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement, Task, TextAlign, TextRun,
    TextStyle, UniformListScrollHandle, Window, WrappedLine, anchored, canvas, deferred, div, fill,
    outline, point, prelude::*, px, rgb, rgba, size, transparent_black, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, IndexPath, Sizable as _, ThemeColor,
    searchable_list::SearchableVec,
    select::{Select, SelectEvent, SelectState},
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
    display_list: RetainedDisplayList,
    bounds: Rect,
    width: f32,
    height: f32,
    rotation: DocumentRotation,
}

struct RegionImageExport<L = RetainedDisplayList> {
    display_list: L,
    page_bounds: Rect,
    region: PageRegion,
    rotation: DocumentRotation,
}

impl<L> RegionImageExport<L> {
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
    render_session: DocumentRenderSession,
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
    display_list: RetainedDisplayList,
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

    fn display_list(&mut self, doc_id: u64, page_idx: usize) -> Option<RetainedDisplayList> {
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
        display_list: RetainedDisplayList,
    ) -> RetainedDisplayList {
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

type TargetLanguageSelectState = SelectState<SearchableVec<TargetLanguageOption>>;

#[derive(Clone)]
struct TargetLanguageSelect {
    state: Entity<TargetLanguageSelectState>,
    trigger_focus: FocusHandle,
}

impl TargetLanguageSelect {
    fn is_open(&self, cx: &gpui::App) -> bool {
        self.state.read(cx).focus_handle(cx) != self.trigger_focus
    }
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
    render_session: DocumentRenderSession,
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
    render_session: DocumentRenderSession,
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
    translation_popup_language_select: Option<TargetLanguageSelect>,
    pinned_translation_language_select: Option<TargetLanguageSelect>,
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

mod ai_context;
mod annotations;
mod chrome;
mod context_menu;
mod controls;
mod destinations;
mod document;
mod epub;
mod export;
mod geometry;
mod highlights;
mod history;
mod hit_test;
mod layout;
mod lifecycle;
mod linked;
mod links;
mod navigation;
mod overview;
mod page_render;
mod paint;
mod panels;
mod position;
mod region;
mod render;
mod search_state;
mod selection;
mod sidebar;
mod text_index;
mod translation;
mod viewport;

impl EventEmitter<PdfViewEvent> for PdfView {}

#[cfg(test)]
mod tests;
