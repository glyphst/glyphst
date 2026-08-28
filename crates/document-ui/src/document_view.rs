use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
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

use crate::ai_panel::AI_PANEL_WIDTH;
use crate::bookmarks::{
    BookmarkPalette, BookmarkPaletteDelegate, BookmarkTitleEditor, BookmarkTitleEditorEvent,
};
use crate::go_to_page::{GoToPagePrompt, GoToPagePromptDelegate};
use crate::history::{HistoryPalette, HistoryPaletteDelegate};
use crate::icons::IconName;
use crate::library::{
    Bookmark, DocumentEditRevision, DocumentPosition, Highlight, HighlightColor, HighlightRect,
    LibraryStore, Note, SemanticTextRange, SidebarWidths, normalize_path,
};
use crate::note_editor::{NoteEditor, NoteEditorEvent};
use crate::notes::{NotePaletteEntry, NotesPalette, NotesPaletteDelegate};
use crate::picker::{Picker, PickerEvent};
use crate::scrollbar::{ManualScrollHandle, ScrollbarHandle, WithScrollbar as _};
use crate::search::{SearchBar, SearchBarSnapshot, SearchEvent};
use crate::settings::{
    BehaviorSettings, CopyFormat, DocumentProgressDisplay, PageImageQuality, PageTextFormat,
    PdfRenderEngine, SettingsStore, TranslationBoxCorner, ui_rems,
};
use crate::smooth_scroll::advance_toward;
use crate::theme::Theme;
use crate::toc::{TocPalette, TocPaletteDelegate, TocPaletteEntry};
use anyhow::{Context as _, Result, anyhow, bail};
use arboard::{Clipboard, ImageData};
pub use glyphst_actions::document_viewer::*;
use glyphst_ai::agent::{
    AgentCancellationToken, AgentImage, AgentToolCall, AgentToolDefinition, AgentToolExecutor,
    AgentToolResult, AgentToolResultPart,
};
use glyphst_ai::ai_chat::{
    ContextBlock, ImageContext, MAX_PAGE_IMAGE_BYTES, MentionKind, MentionReference,
};
use glyphst_ai_ui::{
    AiContext, AiContextProvider, AiPageState, ResolvedPageContext, ResolvedRegionContext,
};
use glyphst_core::{
    DocumentCapabilities, DocumentColors, DocumentFormat, DocumentLayout, DocumentPageLayout,
    DocumentRotation, EpubReaderPreferences, EpubTheme, PageIndex, PageRegion,
    RegionImageReference, SelectedPagesReference, SemanticTextAnchor,
};
use glyphst_files::path_chooser::{SavePathOptions, prompt_for_new_path};
use glyphst_pdf::{
    DocumentPassword, DocumentRenderSession, DocumentSnapshot, ReloadCoordinator,
    ReloadDisposition, RetainedDisplayList,
};
use glyphst_pdf_gpu::{
    CancellationToken as GpuCancellationToken, FeatureId as GpuFeatureId, GpuActionResolutionError,
    GpuDestinationView, GpuFallbackDiagnostic, GpuFallbackSource, GpuLinkAction,
    GpuLinkHighlightMode, GpuLinkInteraction, GpuNamedAction, GpuNavigationRoute, GpuOutlineItem,
    GpuPagePreparationTimings, GpuPagePresentation, GpuPageRegion, GpuPageRenderOptions,
    GpuPageRenderOutcome, GpuPageRoute, GpuPageTexture, GpuPaintRequest,
    GpuPatchLodPreparationStats, GpuPdfDocument, GpuPdfRenderer, GpuPreparationStats,
    GpuRenderStats, GpuWarningDiagnostic, PreparedGpuPage,
};
use glyphst_pdf_translation_service::{
    BlockKind, BoundingBox, PdfTranslationPageTextLayer, PdfTranslationSessionRequest,
    source_text_regions,
};
use glyphst_translation::{LanguageCode, TranslationExecutionConfig, TranslationRequest};
use glyphst_translation_ui::{
    DocumentTranslationId, DocumentTranslationManager, DocumentTranslationOpenMode,
    DocumentTranslationSessionSnapshot, DocumentTranslationSessionStatus,
    LiveTranslationSubmission, TargetLanguageOption, TranslationServices, target_language_options,
};
use glyphst_ui::{
    entry_selection::apply_entry_click,
    format_user_float,
    icons::{Icon, toolbar_icon_button},
    markdown::{Markdown, MarkdownElement, MarkdownStyle},
    motion::{EASE_OUT, HOVER, LAYOUT, MotionSpec, ScalarTween},
    sticky_items::{StickyCandidate, sticky_items},
    style::CONTROL_SIZE,
    tooltip::text_tooltip,
};
use gpui::{
    Anchor, AnyElement, AsyncApp, BorderStyle, Bounds, ClickEvent, ClipboardItem, Context, Corners,
    DismissEvent, DragMoveEvent, Empty, Entity, EventEmitter, ExternalGpuDeviceToken,
    ExternalTextureAlphaMode, ExternalTextureColorEffect, ExternalTextureColorSpace,
    ExternalTextureFilter, ExternalTextureHandle, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement, IntoElement, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point as GpuiPoint, PromptButton, PromptLevel, Render, RenderImage,
    ScrollDelta, ScrollHandle, ScrollStrategy, ScrollWheelEvent, SharedString, Size,
    StatefulInteractiveElement, Task, TextStyle, UniformListScrollHandle, Window, anchored, canvas,
    deferred, div, fill, hsla, linear_color_stop, linear_gradient, outline, point, prelude::*, px,
    rgb, rgba, size, transparent_black, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, IconName as ComponentIconName, IndexPath,
    Selectable as _, Sizable as _, ThemeColor, ThemeStyled as _,
    alert::Alert,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    progress::ProgressCircle,
    searchable_list::SearchableVec,
    select::{Select, SelectEvent, SelectState},
    spinner::Spinner,
};
use image::{Frame, GenericImageView as _, RgbaImage, codecs::jpeg::JpegEncoder};
use mupdf::text_page::{TextBlockType, TextCharFlags};
use mupdf::{
    ColorParams, Colorspace, DestinationKind, Device, DisplayList, Image as PdfImage, Matrix,
    Pixmap, Quad, Rect, TextPageFlags,
};
use serde_json::json;
use smallvec::{SmallVec, smallvec};

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
const MIN_PARALLEL_SHADE_PIXELS: u64 = 1_000_000;
const MIN_RASTER_BAND_HEIGHT: u32 = 256;
const MAX_RASTER_BANDS: usize = 8;
const PAGE_LOADING_INDICATOR_SIZE: f32 = 24.0;
const MAX_JUMP_HISTORY: usize = 128;
const JUMP_POSITION_EPSILON_DOC: f32 = 0.5;

fn pdf_outline_color(color: [f32; 3]) -> Hsla {
    let [red, green, blue] = color.map(|component| {
        (component.clamp(0.0, 1.0) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    });
    rgba(u32::from_be_bytes([red, green, blue, u8::MAX])).into()
}

const TOC_SIDEBAR_WIDTH: f32 = 280.0;
const TOC_SIDEBAR_ROW_HEIGHT: f32 = 30.0;
const BOOKMARKS_SIDEBAR_WIDTH: f32 = 300.0;
const BOOKMARKS_SIDEBAR_ROW_HEIGHT: f32 = 40.0;
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
pub const SIDEBAR_MIN_WIDTH: f32 = 150.0;
const GRID_OVERVIEW_DEFAULT_ZOOM: f32 = 1.0;
const GRID_OVERVIEW_MIN_ZOOM: f32 = 0.25;
const GRID_OVERVIEW_MAX_ZOOM: f32 = 4.0;
const GRID_OVERVIEW_DEFAULT_COLS: usize = 1;
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
const COMPACT_CONTROL_HEIGHT: f32 = 20.0;
const DOCUMENT_FOOTER_HEIGHT: f32 = 32.0;
const DOCUMENT_TOP_POPUP_CLEARANCE: f32 = 44.0;
const TRANSLATION_PANEL_PREFERRED_WIDTH: f32 = 390.0;
const TRANSLATION_PANEL_MARGIN: f32 = 8.0;
const TRANSLATION_PANEL_DEFAULT_RIGHT: f32 = 8.0;
const TRANSLATION_PANEL_DEFAULT_TOP: f32 = DOCUMENT_TOP_POPUP_CLEARANCE + 10.0;
const TRANSLATION_PANEL_COMPACT_BREAKPOINT: f32 = 300.0;
const TRANSLATION_PANEL_DEFAULT_HEIGHT: f32 = 40.0;
const TRANSLATION_PANEL_SNAP_ZONE: f32 = 12.0;
const TRANSLATION_PANEL_EXPOSED_STRIP: f32 = 8.0;
const TRANSLATION_PANEL_INITIAL_DOCK_DELAY: Duration = Duration::from_secs(6);
const TRANSLATION_PANEL_DOCK_DELAY: Duration = Duration::from_secs(2);
const TRANSLATION_PANEL_RETRACT_DELAY: Duration = Duration::from_millis(400);
const TRANSLATION_HOVER_CLEAR_GRACE: Duration = Duration::from_millis(100);
const TRANSLATION_HOVER_FADE: MotionSpec = MotionSpec::new(120, EASE_OUT);
const TRANSLATION_HOVER_TOOLS_OVERLAY_PRIORITY: usize = 4;
const TRANSLATION_PANEL_OVERLAY_PRIORITY: usize = 5;
const TRANSLATION_PANEL_POPOVER_PRIORITY: usize = TRANSLATION_PANEL_OVERLAY_PRIORITY + 1;
const TRANSLATION_PANEL_DRAG_OVERLAY_PRIORITY: usize =
    glyphst_ui::root::ROOT_NOTIFICATION_PRIORITY + 1;

fn translation_panel_width(viewport_width: f32) -> f32 {
    if !viewport_width.is_finite() || viewport_width <= 0.0 {
        return TRANSLATION_PANEL_PREFERRED_WIDTH;
    }
    (viewport_width - TRANSLATION_PANEL_MARGIN - TRANSLATION_PANEL_DEFAULT_RIGHT)
        .clamp(1.0, TRANSLATION_PANEL_PREFERRED_WIDTH)
}

fn default_translation_panel_position(viewport_width: f32, panel_width: f32) -> GpuiPoint<Pixels> {
    point(
        px(
            (viewport_width - panel_width - TRANSLATION_PANEL_DEFAULT_RIGHT)
                .max(TRANSLATION_PANEL_MARGIN),
        ),
        px(TRANSLATION_PANEL_DEFAULT_TOP),
    )
}

fn clamp_translation_panel_position(
    position: GpuiPoint<Pixels>,
    viewport_width: f32,
    viewport_height: f32,
    panel_size: Size<Pixels>,
) -> GpuiPoint<Pixels> {
    let max_x = (viewport_width - panel_size.width.as_f32() - TRANSLATION_PANEL_MARGIN)
        .max(TRANSLATION_PANEL_MARGIN);
    let max_y = (viewport_height - panel_size.height.as_f32() - TRANSLATION_PANEL_MARGIN)
        .max(TRANSLATION_PANEL_MARGIN);
    point(
        px(position.x.as_f32().clamp(TRANSLATION_PANEL_MARGIN, max_x)),
        px(position.y.as_f32().clamp(TRANSLATION_PANEL_MARGIN, max_y)),
    )
}

fn translation_panel_snap_side(
    position: GpuiPoint<Pixels>,
    panel_width: f32,
    viewport_width: f32,
) -> Option<TranslationPanelSide> {
    let left_distance = position.x.as_f32().max(0.0);
    let right_distance = (viewport_width - (position.x.as_f32() + panel_width)).max(0.0);
    match (
        left_distance <= TRANSLATION_PANEL_SNAP_ZONE,
        right_distance <= TRANSLATION_PANEL_SNAP_ZONE,
    ) {
        (true, true) if right_distance < left_distance => Some(TranslationPanelSide::Right),
        (true, _) => Some(TranslationPanelSide::Left),
        (_, true) => Some(TranslationPanelSide::Right),
        _ => None,
    }
}

fn translation_panel_docked_x(
    side: TranslationPanelSide,
    reveal: f32,
    viewport_width: f32,
    panel_width: f32,
) -> Pixels {
    let reveal = reveal.clamp(0.0, 1.0);
    let shown = match side {
        TranslationPanelSide::Left => TRANSLATION_PANEL_MARGIN,
        TranslationPanelSide::Right => {
            (viewport_width - panel_width - TRANSLATION_PANEL_MARGIN).max(TRANSLATION_PANEL_MARGIN)
        }
    };
    let hidden = match side {
        TranslationPanelSide::Left => -panel_width + TRANSLATION_PANEL_EXPOSED_STRIP,
        TranslationPanelSide::Right => viewport_width - TRANSLATION_PANEL_EXPOSED_STRIP,
    };
    px(hidden + (shown - hidden) * reveal)
}

fn clamp_translation_panel_edge_y(y: Pixels, viewport_height: f32, panel_height: f32) -> Pixels {
    let maximum =
        (viewport_height - panel_height - TRANSLATION_PANEL_MARGIN).max(TRANSLATION_PANEL_MARGIN);
    px(y.as_f32().clamp(TRANSLATION_PANEL_MARGIN, maximum))
}

fn translation_hover_interaction_bounds(
    highlight_bounds: Option<Bounds<Pixels>>,
    tools_bounds: Bounds<Pixels>,
) -> Bounds<Pixels> {
    highlight_bounds.map_or(tools_bounds, |bounds| bounds.union(&tools_bounds))
}

static NEXT_DOCUMENT_ID: AtomicU64 = AtomicU64::new(1);

fn next_document_id() -> u64 {
    NEXT_DOCUMENT_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone)]
struct DocumentPage {
    bounds: Rect,
    width: f32,
    height: f32,
    offset_x: f32,
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
        DocumentView::pdf_rect_to_view(
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
    pages: Vec<DocumentPage>,
    max_page_width: f32,
}

struct LoadedPages {
    pages: Vec<DocumentPage>,
    max_page_width: f32,
    content_height_doc: f32,
}

struct TextChunk {
    start_page_idx: usize,
    glyphs: Vec<Glyph>,
    text_blocks: Vec<TextBlock>,
    text_lines: Vec<TextLine>,
    links: Vec<DocumentLink>,
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
    links: Vec<DocumentLink>,
    page_glyph_ranges: Vec<Range<usize>>,
    page_line_ranges: Vec<Range<usize>>,
    page_block_ranges: Vec<Range<usize>>,
}

struct ReloadData {
    snapshot: Arc<DocumentSnapshot>,
    render_session: DocumentRenderSession,
    password_protected: bool,
    page_count: usize,
    loaded: LoadedPages,
    toc: Vec<TocEntry>,
}

#[derive(Clone)]
struct RevisionedPageRenderSource {
    revision: u64,
    text_revision: Option<u64>,
    // The render session borrows the immutable bytes indirectly, but retaining
    // the snapshot here makes that lifetime explicit at the view boundary.
    _snapshot: Arc<DocumentSnapshot>,
    render_session: DocumentRenderSession,
}

struct PreparedPageText {
    glyphs: Vec<Glyph>,
    text_blocks: Vec<TextBlock>,
    text_lines: Vec<TextLine>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileSignature {
    modified: Option<SystemTime>,
    len: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RenderKey {
    doc_id: u64,
    page_idx: usize,
    source_revision: u64,
    rotation: DocumentRotation,
    color_transform: Option<PdfColorTransform>,
    physical_width: u32,
    physical_height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PdfColorTransform {
    background: u32,
    foreground: u32,
}

impl From<DocumentColors> for PdfColorTransform {
    fn from(colors: DocumentColors) -> Self {
        let colors = colors.sanitized();
        Self {
            background: colors.background,
            foreground: colors.foreground,
        }
    }
}

impl RenderKey {
    fn pixel_count(self) -> u64 {
        self.physical_width as u64 * self.physical_height as u64
    }

    fn byte_len(self) -> u64 {
        self.pixel_count().saturating_mul(4)
    }
}

/// Process-wide raster-cache diagnostics used by the rendering debug HUD.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderCacheDebugSnapshot {
    pub raster_bytes: u64,
    pub raster_count: usize,
    pub raster_byte_limit: u64,
    pub protected_raster_count: usize,
    pub display_list_count: usize,
    pub display_list_limit: usize,
}

/// How closely the texture currently painted for a page matches the requested
/// viewport texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageRenderDebugStatus {
    Exact,
    RetainedGpu { exact_pending: bool },
    Fallback { exact_pending: bool },
    Pending,
    Failed,
    Unavailable,
}

/// Text-layer availability for the active page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextIndexDebugState {
    Pending,
    Indexing,
    Ready,
    Failed,
}

/// Exact, known allocation size for an RGBA page raster.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterDebugSnapshot {
    pub physical_width: u32,
    pub physical_height: u32,
    pub bytes: u64,
    pub scale: f32,
}

/// Stage timing for the last completed render attempt for a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRenderTimingDebugSnapshot {
    /// Request-to-cache wall time, including executor queueing and UI handoff.
    pub total: Duration,
    /// Page loading and display-list recording; absent when the list was cached.
    pub display_list: Option<Duration>,
    pub display_list_cached: bool,
    /// Complete bitmap stage, including MuPDF drawing and Glyphst post-processing.
    pub raster: Option<Duration>,
    /// Wall-clock envelope of the MuPDF display-list playback call(s).
    pub mupdf_draw: Option<Duration>,
    /// Number of independently rasterized horizontal bands.
    pub raster_bands: Option<usize>,
    pub succeeded: bool,
}

/// Complete-page renderer currently presented for an active PDF page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageRenderBackendDebug {
    MuPdf,
    GlyphstGpu,
    GlyphstPending,
    GlyphstFailed,
    MuPdfWhileGpuPending,
    MuPdfFallback,
}

/// Metrics emitted by the retained native GPU renderer for its latest frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuPageRenderDebugSnapshot {
    pub cpu_time: Duration,
    pub frame_pool_wait_time: Duration,
    pub command_encoding_time: Duration,
    pub gpu_time: Option<Duration>,
    pub host_to_device_bytes: u64,
    pub passes_executed: u64,
    pub command_buffers: u64,
    pub vector_batches: u64,
    pub procedural_stroke_batches: u64,
    pub render_bundle_runs: u64,
    pub reprojected_frame: bool,
    pub gradient_draws: u64,
    pub image_draws: u64,
    pub patch_lod_required_scale: String,
    pub patch_lod_available_scale: String,
    pub patch_lod_deferred: bool,
    pub reused_tiles: u64,
    pub rendered_tiles: u64,
    pub dirty_tiles: u64,
    pub refinement_pending: bool,
    pub selected_vector_lod: u32,
    pub frame_pool_reused: bool,
    pub frame_pool_grew: bool,
    pub frame_pool_capacity_bytes: u64,
    pub clip_state_changes: u64,
    pub stencil_draws: u64,
    pub compute_dispatches: u64,
    pub offscreen_passes: u64,
    pub mask_passes: u64,
    pub composite_passes: u64,
    pub backdrop_copies: u64,
    pub transient_peak_bytes: u64,
    pub transient_resident_bytes: u64,
    pub transient_pool_reuses: u64,
    pub generation: u64,
    pub coverage_strategy: String,
}

/// One page's retained preparation and residency accounting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuPagePreparationDebugSnapshot {
    pub frontend_preflight: Duration,
    pub compile: Duration,
    pub link_conversion: Duration,
    pub gpu_prepare: Duration,
    pub total: Duration,
    pub compiled_cache_hit: bool,
    pub elapsed: Duration,
    pub host_to_device_bytes: u64,
    pub resident_bytes: u64,
    pub image_textures: u64,
    pub alpha_textures: u64,
    pub image_mip_levels: u64,
    pub alpha_mip_levels: u64,
    pub image_texture_bytes: u64,
    pub mip_texture_bytes: u64,
    pub gradient_tables: u64,
    pub patch_shading_lod_levels: u64,
    pub patch_shading_deferred_lod_levels: u64,
    pub patch_shading_mesh_bytes: u64,
    pub deferred_patch_lod_elapsed: Duration,
    pub deferred_patch_lod_levels: u64,
    pub deferred_patch_lod_bytes: u64,
    pub vector_batches: u64,
    pub vector_lod_levels: u64,
    pub vector_lod_compacted: bool,
    pub vector_vertices: u64,
    pub vector_triangle_references: u64,
    pub vector_reference_bytes: u64,
    pub vector_compact_reference_lods: u64,
    pub vector_indexed_reference_lods: u64,
    pub vector_canonical_indices: u64,
    pub vector_reference_segments: u64,
    pub vector_instances: u64,
    pub vector_bytes: u64,
    pub procedural_stroke_batches: u64,
    pub hairline_lod_levels: u64,
    pub hairline_geometry_records: u64,
    pub stroke_state_records: u64,
    pub hairline_primitives: u64,
    pub hairline_draws: u64,
    pub hairline_bytes: u64,
    pub hairline_chunks: u64,
    pub procedural_stroke_lod_levels: u64,
    pub procedural_stroke_primitives: u64,
    pub procedural_stroke_draws: u64,
    pub procedural_stroke_bytes: u64,
    pub render_bundle_runs: u64,
    pub render_bundle_operations: u64,
    pub retained_operations: u64,
    pub flattened_opaque_knockout_groups: u64,
    pub retained_form_resources: u64,
    pub retained_form_instances: u64,
    pub retained_type3_glyph_resources: u64,
    pub retained_type3_glyph_instances: u64,
    pub retained_tiling_pattern_resources: u64,
    pub retained_tiling_pattern_instances: u64,
    pub expanded_tiling_pattern_cells: u64,
    pub retained_transparency_groups: u64,
    pub retained_soft_mask_resources: u64,
    pub retained_soft_mask_instances: u64,
    pub retained_subscene_nodes: u64,
    pub expanded_scene_nodes: u64,
    pub unique_clip_paths: u64,
    pub analytic_paths: u64,
    pub analytic_segments: u64,
    pub analytic_draws: u64,
    pub analytic_groups: u64,
    pub analytic_bvh_nodes: u64,
    pub analytic_bytes: u64,
    pub analytic_fallback: Option<String>,
}

/// Bounded native reader/GPU cache occupancy for the current document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuCacheDebugSnapshot {
    pub frontend_outline_fonts: usize,
    pub frontend_outline_font_bytes: u64,
    pub frontend_outline_font_hits: u64,
    pub frontend_outline_font_misses: u64,
    pub frontend_outline_font_evictions: u64,
    pub frontend_objects: usize,
    pub frontend_object_hits: u64,
    pub frontend_object_misses: u64,
    pub frontend_object_evictions: u64,
    pub frontend_preflight_reports: usize,
    pub frontend_preflight_scans: u64,
    pub compiled_pages: usize,
    pub compiled_page_limit: usize,
    pub content_bound_pages: usize,
    pub content_bound_page_limit: usize,
    pub prepared_pages: usize,
    pub prepared_page_limit: usize,
    pub resident_bytes: u64,
    pub resident_byte_limit: u64,
    pub surface_count: usize,
    pub surface_bytes: u64,
    pub surface_byte_limit: u64,
    pub continuity_surface_count: usize,
    pub continuity_surface_bytes: u64,
    pub surface_seed_tasks: usize,
    pub surface_in_flight_bytes: u64,
    pub surface_render_pending: usize,
    pub surface_pending_peak: usize,
    pub surface_renders_started: u64,
    pub surface_renders_completed: u64,
    pub surface_renders_coalesced: u64,
    pub surface_renders_superseded: u64,
    pub surface_renders_deferred: u64,
    pub surface_renders_discarded: u64,
    pub surface_render_cache_pressure: u64,
    pub surface_scroll_reuses: u64,
    pub surface_scroll_prefetches: u64,
    pub surface_scroll_misses: u64,
    pub surface_zoom_reuses: u64,
    pub surface_zoom_quality_refreshes: u64,
    pub surface_zoom_coverage_refreshes: u64,
    pub surface_zoom_settled_upgrades: u64,
    pub surface_warm_requests: u64,
    pub surface_warm_hits: u64,
    pub surface_warm_pages: usize,
    pub surface_continuity_requests: u64,
    pub surface_continuity_completed: u64,
    pub surface_continuity_hits: u64,
    pub surface_partial_reuses: u64,
    pub surface_presentation_gaps: u64,
    pub surface_queue_latency_p95: Duration,
    pub surface_ready_latency_p95: Duration,
    pub surface_present_latency_p95: Duration,
    pub refinement_pages: usize,
    pub active_preparations: usize,
    pub queued_preparations: usize,
    pub speculative_preparations: usize,
    pub completed_preparations: u64,
    pub cancelled_preparations: u64,
    pub motion_active: bool,
}

/// One exact semantic or capability reason for routing the whole page to MuPDF.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuFallbackDebugSnapshot {
    pub feature: String,
    pub code: String,
    pub message: String,
    pub source: &'static str,
}

/// One deterministic native frontend repair that did not require fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuWarningDebugSnapshot {
    pub feature: String,
    pub code: String,
    pub message: String,
}

/// Rendering and content diagnostics for one active page.
#[derive(Clone, Debug, PartialEq)]
pub struct PageRenderDebugSnapshot {
    pub page_number: usize,
    pub status: PageRenderDebugStatus,
    pub backend: PageRenderBackendDebug,
    pub gpu: Option<GpuPageRenderDebugSnapshot>,
    pub gpu_preparation: Option<GpuPagePreparationDebugSnapshot>,
    pub gpu_fallbacks: Vec<GpuFallbackDebugSnapshot>,
    pub gpu_warnings: Vec<GpuWarningDebugSnapshot>,
    pub gpu_failure: Option<String>,
    pub requested_raster: Option<RasterDebugSnapshot>,
    pub displayed_raster: Option<RasterDebugSnapshot>,
    pub cached_raster_bytes: u64,
    pub cached_raster_variants: usize,
    pub source_revision: u64,
    pub latest_timing: Option<PageRenderTimingDebugSnapshot>,
    pub glyph_count: usize,
    pub line_count: usize,
    pub link_count: usize,
    pub text_index_state: TextIndexDebugState,
}

/// Reader interaction state captured alongside rendering diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentInteractionDebugSnapshot {
    pub source_path: PathBuf,
    pub configured_engine: String,
    pub document_layout: String,
    pub page_layout: String,
    pub rotation: String,
    pub grid_overview_open: bool,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub zoom: f32,
    pub target_zoom: f32,
    pub effective_scale: f32,
    pub scroll_x_doc: f32,
    pub scroll_y_doc: f32,
    pub target_scroll_x_doc: f32,
    pub target_scroll_y_doc: f32,
    pub max_scroll_y_doc: f32,
    pub motion_active: bool,
}

/// Rendering diagnostics for the active document viewport.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentRenderDebugSnapshot {
    pub interaction: DocumentInteractionDebugSnapshot,
    pub page_label: String,
    pub pages: Vec<PageRenderDebugSnapshot>,
    pub pending_render_count: usize,
    pub pending_render_limit: usize,
    pub gpu_cache: GpuCacheDebugSnapshot,
    /// Document-level reasons for retaining MuPDF navigation. These never
    /// change the rendering backend of a supported page.
    pub gpu_navigation_fallbacks: Vec<GpuFallbackDebugSnapshot>,
}

#[derive(Clone, Copy)]
struct ViewportPageRenderDebug {
    requested: RenderKey,
    displayed: Option<RenderKey>,
}

#[derive(Clone, Copy)]
struct PageRenderAttemptDebug {
    key: RenderKey,
    total: Duration,
    display_list: Option<Duration>,
    display_list_cached: bool,
    raster: Option<Duration>,
    mupdf_draw: Option<Duration>,
    raster_bands: Option<usize>,
    succeeded: bool,
}

#[derive(Clone, Copy)]
enum PageOverviewSurface {
    ThumbnailSidebar,
    GridOverview,
}

#[derive(Default)]
struct SurfaceRenderKeys {
    by_page: HashMap<usize, RenderKey>,
}

impl SurfaceRenderKeys {
    fn register(&mut self, page_idx: usize, key: RenderKey) -> bool {
        debug_assert_eq!(key.page_idx, page_idx);
        self.by_page.insert(page_idx, key) != Some(key)
    }

    fn retain_pages(&mut self, visible_pages: impl IntoIterator<Item = usize>) -> bool {
        let visible_pages = visible_pages.into_iter().collect::<HashSet<_>>();
        let previous_len = self.by_page.len();
        self.by_page
            .retain(|page_idx, _| visible_pages.contains(page_idx));
        self.by_page.len() != previous_len
    }

    fn values(&self) -> impl Iterator<Item = RenderKey> + '_ {
        self.by_page.values().copied()
    }

    fn clear(&mut self) {
        self.by_page.clear();
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
    source_revision: u64,
    display_list: RetainedDisplayList,
    last_access: u64,
}

#[derive(Default)]
pub struct RenderCache {
    renders: Vec<CachedRender>,
    display_lists: Vec<CachedDisplayList>,
    protected_renders: HashMap<u64, HashSet<RenderKey>>,
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

    fn set_protected_renders(&mut self, doc_id: u64, renders: impl IntoIterator<Item = RenderKey>) {
        let renders = renders
            .into_iter()
            .filter(|key| key.doc_id == doc_id)
            .collect::<HashSet<_>>();
        if renders.is_empty() {
            self.protected_renders.remove(&doc_id);
        } else {
            self.protected_renders.insert(doc_id, renders);
        }
    }

    fn clear_protected_renders(&mut self, doc_id: u64) {
        self.protected_renders.remove(&doc_id);
    }

    fn is_render_protected(&self, key: RenderKey) -> bool {
        self.protected_renders
            .get(&key.doc_id)
            .is_some_and(|renders| renders.contains(&key))
    }

    fn render(&mut self, key: RenderKey) -> Option<Arc<RenderImage>> {
        self.exact_render_index(key)
            .map(|index| self.touch_render(index))
    }

    fn scaled_render(
        &mut self,
        doc_id: u64,
        requested: RenderKey,
        render_scale: f32,
    ) -> Option<Arc<RenderImage>> {
        self.scaled_render_with_key(doc_id, requested, render_scale)
            .map(|(_, image)| image)
    }

    fn scaled_render_with_key(
        &mut self,
        doc_id: u64,
        requested: RenderKey,
        render_scale: f32,
    ) -> Option<(RenderKey, Arc<RenderImage>)> {
        let index = self
            .renders
            .iter()
            .enumerate()
            .filter(|(_, cached)| {
                cached.key.doc_id == doc_id
                    && cached.key.page_idx == requested.page_idx
                    && cached.key.rotation == requested.rotation
                    && cached.key.color_transform == requested.color_transform
            })
            .min_by(|(_, a), (_, b)| {
                (a.render_scale - render_scale)
                    .abs()
                    .total_cmp(&(b.render_scale - render_scale).abs())
            })
            .map(|(index, _)| index)?;

        let key = self.renders[index].key;
        Some((key, self.touch_render(index)))
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
                .filter(|(_, cached)| !self.is_render_protected(cached.key))
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

    fn display_list(
        &mut self,
        doc_id: u64,
        page_idx: usize,
        source_revision: u64,
    ) -> Option<RetainedDisplayList> {
        let index = self.display_lists.iter().position(|cached| {
            cached.doc_id == doc_id
                && cached.page_idx == page_idx
                && cached.source_revision == source_revision
        })?;
        let last_access = self.next_access();
        self.display_lists[index].last_access = last_access;
        Some(self.display_lists[index].display_list.clone())
    }

    fn insert_display_list(
        &mut self,
        doc_id: u64,
        page_idx: usize,
        source_revision: u64,
        display_list: RetainedDisplayList,
    ) -> RetainedDisplayList {
        let last_access = self.next_access();

        if let Some(index) = self.display_lists.iter().position(|cached| {
            cached.doc_id == doc_id
                && cached.page_idx == page_idx
                && cached.source_revision == source_revision
        }) {
            self.display_lists[index] = CachedDisplayList {
                doc_id,
                page_idx,
                source_revision,
                display_list: display_list.clone(),
                last_access,
            };
        } else {
            self.display_lists.push(CachedDisplayList {
                doc_id,
                page_idx,
                source_revision,
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
        self.protected_renders.remove(&doc_id);
        removed
    }

    fn evict_page(&mut self, doc_id: u64, page_idx: usize) -> Vec<Arc<RenderImage>> {
        let mut removed = Vec::new();
        self.renders.retain(|cached| {
            if cached.key.doc_id == doc_id && cached.key.page_idx == page_idx {
                removed.push(cached.image.clone());
                false
            } else {
                true
            }
        });
        self.display_lists
            .retain(|cached| cached.doc_id != doc_id || cached.page_idx != page_idx);
        if let Some(protected) = self.protected_renders.get_mut(&doc_id) {
            protected.retain(|key| key.page_idx != page_idx);
        }
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

    fn debug_snapshot(&self) -> RenderCacheDebugSnapshot {
        RenderCacheDebugSnapshot {
            raster_bytes: self.render_pixels().saturating_mul(4),
            raster_count: self.renders.len(),
            raster_byte_limit: MAX_CACHED_RENDER_PIXELS.saturating_mul(4),
            protected_raster_count: self
                .renders
                .iter()
                .filter(|cached| self.is_render_protected(cached.key))
                .count(),
            display_list_count: self.display_lists.len(),
            display_list_limit: MAX_CACHED_DISPLAY_LISTS,
        }
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
    page_idx: usize,
    bounds: Bounds<Pixels>,
    image: Option<Arc<RenderImage>>,
    gpu_texture: Option<GpuPaintTexture>,
}

#[derive(Clone)]
struct GpuPaintTexture {
    bounds: Bounds<Pixels>,
    texture: GpuPageTexture,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GpuSurfaceIntent {
    Visible,
    AdjacentWarm,
}

type HighlightOverlay = (Bounds<Pixels>, HighlightColor);

struct LinkInvertOverlay {
    bounds: Bounds<Pixels>,
    source_bounds: Bounds<f32>,
    texture: ExternalTextureHandle,
}

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
    link_invert_overlays: Vec<LinkInvertOverlay>,
    diff_flash_overlays: Vec<DiffFlashOverlay>,
    highlight_overlays: Vec<HighlightOverlay>,
    selected_highlight_borders: Vec<Bounds<Pixels>>,
    note_underlines: Vec<(Bounds<Pixels>, HighlightColor)>,
    link_highlights: Vec<Bounds<Pixels>>,
    linked_hover_highlights: Vec<TranslationHoverHighlight>,
    failed_translation_highlights: Vec<Bounds<Pixels>>,
    selection_highlights: Vec<Bounds<Pixels>>,
    search_highlights: Vec<Bounds<Pixels>>,
    active_search_highlights: Vec<Bounds<Pixels>>,
    region_overlays: Vec<Bounds<Pixels>>,
}

struct TranslationHoverAnimation {
    region_id: String,
    x: ScalarTween,
    y: ScalarTween,
    width: ScalarTween,
    height: ScalarTween,
    opacity: ScalarTween,
}

impl TranslationHoverAnimation {
    fn new(region_id: String, target: Bounds<Pixels>) -> Self {
        Self {
            region_id,
            x: ScalarTween::new(target.origin.x.as_f32(), HOVER),
            y: ScalarTween::new(target.origin.y.as_f32(), HOVER),
            width: ScalarTween::new(target.size.width.as_f32(), HOVER),
            height: ScalarTween::new(target.size.height.as_f32(), HOVER),
            opacity: ScalarTween::new(1.0, TRANSLATION_HOVER_FADE),
        }
    }

    fn retarget(
        &mut self,
        region_id: String,
        target: Bounds<Pixels>,
        now: Instant,
        reduce_motion: bool,
    ) {
        self.region_id = region_id;
        self.x
            .retarget(target.origin.x.as_f32(), now, reduce_motion);
        self.y
            .retarget(target.origin.y.as_f32(), now, reduce_motion);
        self.width
            .retarget(target.size.width.as_f32(), now, reduce_motion);
        self.height
            .retarget(target.size.height.as_f32(), now, reduce_motion);
        self.opacity.retarget(1.0, now, reduce_motion);
    }

    fn fade_out(&mut self, now: Instant, reduce_motion: bool) {
        self.opacity.retarget(0.0, now, reduce_motion);
    }

    fn bounds(&self, now: Instant) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.x.value(now)), px(self.y.value(now))),
            size(px(self.width.value(now)), px(self.height.value(now))),
        )
    }

    fn is_animating(&self, now: Instant) -> bool {
        [self.x, self.y, self.width, self.height, self.opacity]
            .into_iter()
            .any(|tween| tween.is_animating(now))
    }
}

#[derive(Clone, Copy)]
struct TranslationHoverHighlight {
    bounds: Bounds<Pixels>,
    opacity: f32,
}

#[derive(Clone)]
struct TranslationHoverRegionEntry {
    page_index: usize,
    unit_id: Option<String>,
    block_id: Option<String>,
    target_bounds: BoundingBox,
    source_regions: Vec<BoundingBox>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranslationPanelSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranslationPanelDockSurface {
    Panel,
    Edge,
}

#[derive(Clone, Copy, Debug, Default)]
enum TranslationPanelPlacement {
    #[default]
    Default,
    Floating(GpuiPoint<Pixels>),
    PendingDock {
        position: GpuiPoint<Pixels>,
        side: TranslationPanelSide,
        y: Pixels,
    },
    Docked {
        side: TranslationPanelSide,
        y: Pixels,
        reveal: ScalarTween,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TranslationPanelDrag {
    Floating { pointer_offset: GpuiPoint<Pixels> },
}

#[derive(Clone)]
struct TranslationPanelDragToken(u64);

impl Render for TranslationPanelDragToken {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DocumentViewLanguage {
    Original,
    Translated,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TranslationViewRole {
    #[default]
    None,
    /// The live translated page. Pointer movement here is the sole initiator
    /// of semantic paired highlighting.
    Translation,
    /// The source half of a live pair. It only mirrors semantic hover state.
    SourceMirror,
}

fn translation_pair_highlights_enabled(
    role: TranslationViewRole,
    pair_active: bool,
    overlay_enabled: bool,
) -> bool {
    pair_active && overlay_enabled && role != TranslationViewRole::None
}

fn translation_toolbar_enabled(role: TranslationViewRole, pair_active: bool) -> bool {
    pair_active && role == TranslationViewRole::Translation
}

#[allow(clippy::too_many_arguments)]
fn translation_panel_initial_dock_should_run(
    armed: bool,
    role: TranslationViewRole,
    pair_active: bool,
    has_session: bool,
    at_default_position: bool,
    dragging: bool,
    panel_hovered: bool,
    menu_open: bool,
) -> bool {
    armed
        && role == TranslationViewRole::Translation
        && pair_active
        && has_session
        && at_default_position
        && !dragging
        && !panel_hovered
        && !menu_open
}

fn translation_panel_should_retain_expanded(
    panel_hovered: bool,
    edge_hovered: bool,
    owned_surface_active: bool,
) -> bool {
    panel_hovered || edge_hovered || owned_surface_active
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SynchronizedScrollMode {
    Immediate,
    Target,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SynchronizedScrollDelta {
    pub x_pixels: f32,
    pub y_pixels: f32,
    pub mode: SynchronizedScrollMode,
}

/// An absolute fixed-layout PDF location used by the two halves of a live
/// translation pair. Coordinates are expressed in the original, unrotated PDF
/// page space so each view can preserve its own zoom and rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TranslationViewportAnchor {
    pub page_index: usize,
    pub pdf_x: f32,
    pub pdf_y: f32,
    /// Vertical placement of the PDF point in the viewport: `0.0` is the top,
    /// `0.5` is the center, and `1.0` is the bottom. Boundary placement keeps
    /// differently sized or zoomed viewports aligned without overscrolling.
    pub viewport_y_fraction: f32,
    pub mode: SynchronizedScrollMode,
}

fn translation_viewport_anchors_match(
    left: TranslationViewportAnchor,
    right: TranslationViewportAnchor,
) -> bool {
    left.page_index == right.page_index
        && (left.pdf_x - right.pdf_x).abs() < 0.01
        && (left.pdf_y - right.pdf_y).abs() < 0.01
        && (left.viewport_y_fraction - right.viewport_y_fraction).abs() < 0.001
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    anchor: usize,
    head: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimarySelectionPress {
    Empty,
    Highlight(u64),
    TextAnchor {
        glyph_ix: usize,
        highlight_id: Option<u64>,
    },
    Word(usize),
}

fn primary_selection_press(
    highlight_id: Option<u64>,
    glyph_ix: Option<usize>,
    click_count: usize,
) -> PrimarySelectionPress {
    if let Some(glyph_ix) = glyph_ix {
        if click_count == 2 {
            PrimarySelectionPress::Word(glyph_ix)
        } else {
            PrimarySelectionPress::TextAnchor {
                glyph_ix,
                highlight_id,
            }
        }
    } else if let Some(highlight_id) = highlight_id {
        PrimarySelectionPress::Highlight(highlight_id)
    } else {
        PrimarySelectionPress::Empty
    }
}

fn extend_text_selection(
    selection: &mut Option<Selection>,
    selected_highlight: &mut Option<u64>,
    pending_anchor: Option<usize>,
    head: usize,
) -> bool {
    let Some(mut next_selection) = (*selection).or_else(|| {
        pending_anchor.map(|anchor| Selection {
            anchor,
            head: anchor,
        })
    }) else {
        return false;
    };

    next_selection.head = head;
    *selection = Some(next_selection);
    *selected_highlight = None;
    true
}

fn context_highlight_target(
    highlight_id: Option<u64>,
    pointer_in_text_selection: bool,
) -> Option<u64> {
    if pointer_in_text_selection {
        None
    } else {
        highlight_id
    }
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
    allow_text_selection: bool,
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

pub struct DocumentAnnotations {
    pub highlights: Vec<Highlight>,
    pub notes: Vec<Note>,
}

pub struct DocumentOpenOptions {
    pub initial_position: Option<DocumentPosition>,
    pub annotations: DocumentAnnotations,
    pub target_language: String,
    pub epub_preferences: EpubReaderPreferences,
    pub rotation: DocumentRotation,
    pub page_layout: DocumentPageLayout,
    pub document_translation_jobs: Entity<DocumentTranslationManager>,
}

pub struct PreparedDocumentOpen {
    path: PathBuf,
    file_name: SharedString,
    format: DocumentFormat,
    password_protected: bool,
    capabilities: DocumentCapabilities,
    document_layout: DocumentLayout,
    epub_preferences: EpubReaderPreferences,
    document_colors: Option<DocumentColors>,
    document_snapshot: Arc<DocumentSnapshot>,
    render_session: DocumentRenderSession,
    pages: Vec<DocumentPage>,
    page_count: usize,
    max_page_width: f32,
    content_height_doc: f32,
    page_gap: f32,
    toc: Vec<TocEntry>,
}

pub enum DocumentViewEvent {
    OpenDocument {
        path: PathBuf,
        position: Option<DocumentPosition>,
    },
    OpenSettings,
    SidebarNavigationRequested,
    SynchronizedScroll(SynchronizedScrollDelta),
    /// A canonical content-region id emitted only by the active translated
    /// half of a live session pair.
    TranslationRegionHovered {
        session_id: DocumentTranslationId,
        region_id: Option<String>,
    },
    TranslationOverlayVisibilityChanged {
        session_id: DocumentTranslationId,
        enabled: bool,
    },
    TranslationViewportChanged {
        session_id: DocumentTranslationId,
        anchor: TranslationViewportAnchor,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchMatch {
    page_idx: usize,
    range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PageSearchMatch {
    match_ix: usize,
    range: Range<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SearchTextMapping {
    byte_offset: usize,
    glyph_ix: Option<usize>,
}

struct SearchTextIndex {
    text: String,
    mapping: Vec<SearchTextMapping>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SearchHighlightRect {
    rect: HighlightRect,
    active: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct PageThumbnailOverlays {
    highlights: Vec<(HighlightRect, HighlightColor)>,
    note_underlines: Vec<(HighlightRect, HighlightColor)>,
    search_highlights: Vec<SearchHighlightRect>,
}

struct SearchState {
    query: SharedString,
    matches: Vec<SearchMatch>,
    page_matches: Vec<Vec<PageSearchMatch>>,
    active: Option<usize>,
    ignore_case: bool,
    regex_enabled: bool,
    regex_error: Option<SharedString>,
    search_text: Option<SearchTextIndex>,
    reveal_after_indexing: bool,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: "".into(),
            matches: Vec::new(),
            page_matches: Vec::new(),
            active: None,
            ignore_case: true,
            regex_enabled: false,
            regex_error: None,
            search_text: None,
            reveal_after_indexing: false,
        }
    }
}

impl SearchState {
    fn clear_results(&mut self, page_count: usize) {
        self.matches.clear();
        self.active = None;
        self.regex_error = None;
        for page_matches in &mut self.page_matches {
            page_matches.clear();
        }
        self.page_matches.truncate(page_count);
        self.page_matches.resize_with(page_count, Vec::new);
    }

    fn invalidate_search_text(&mut self) {
        self.search_text = None;
    }

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

#[derive(Clone)]
struct DocumentLink {
    page_idx: usize,
    rects: Vec<Rect>,
    target: LinkTarget,
    pdf_annotation: bool,
    native_interaction_ix: Option<usize>,
}

#[derive(Clone)]
struct TocEntry {
    title: SharedString,
    depth: usize,
    parent_ix: Option<usize>,
    has_children: bool,
    page_idx: Option<usize>,
    target_top: Option<f32>,
    target_left: Option<f32>,
    navigable: bool,
    native_action: Option<GpuLinkAction>,
    initially_open: bool,
    color: Option<[f32; 3]>,
    italic: bool,
    bold: bool,
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
    presentation_page: Option<usize>,
}

#[derive(Clone)]
enum BookmarkTitleTarget {
    New(DocumentPosition),
    Existing(u64),
}

#[derive(Clone)]
struct BookmarkTitleEdit {
    target: BookmarkTitleTarget,
    automatic_title: String,
    initially_automatic: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentSidebar {
    Thumbnails,
    Toc,
    Bookmarks,
    Notes,
}

impl DocumentSidebar {
    pub fn width(self, widths: SidebarWidths) -> f32 {
        match self {
            Self::Thumbnails => widths.thumbnails,
            Self::Toc => widths.toc,
            Self::Bookmarks => widths.bookmarks,
            Self::Notes => widths.notes,
        }
    }

    pub fn set_width(self, widths: &mut SidebarWidths, width: f32) {
        match self {
            Self::Thumbnails => widths.thumbnails = width,
            Self::Toc => widths.toc = width,
            Self::Bookmarks => widths.bookmarks = width,
            Self::Notes => widths.notes = width,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocumentDockSnapshot {
    pub left_sidebar: Option<DocumentSidebar>,
    pub ai_panel_open: bool,
    pub tool_rail_width: f32,
    pub widths: SidebarWidths,
    /// The workspace is continuously changing the document viewport as a dock
    /// opens, closes, or switches width. PDF geometry still follows the live
    /// viewport, but raster requests are held at a cached size until motion
    /// settles so intermediate full-screen renders cannot evict one another.
    pub layout_motion_active: bool,
}

impl Default for DocumentDockSnapshot {
    fn default() -> Self {
        Self {
            left_sidebar: None,
            ai_panel_open: false,
            tool_rail_width: 0.0,
            widths: default_sidebar_widths(),
            layout_motion_active: false,
        }
    }
}

impl DocumentDockSnapshot {
    pub fn occupied_width(self) -> f32 {
        let left = self
            .left_sidebar
            .map(|sidebar| sidebar.width(self.widths) + 1.0)
            .unwrap_or_default();
        let right = if self.ai_panel_open {
            self.widths.ai + 1.0
        } else {
            0.0
        };
        self.tool_rail_width.max(0.0) + left + right
    }
}

pub fn default_sidebar_widths() -> SidebarWidths {
    SidebarWidths {
        thumbnails: THUMBNAIL_SIDEBAR_WIDTH,
        toc: TOC_SIDEBAR_WIDTH,
        bookmarks: BOOKMARKS_SIDEBAR_WIDTH,
        notes: NOTES_SIDEBAR_WIDTH,
        ai: AI_PANEL_WIDTH,
    }
}

pub fn sanitize_sidebar_widths(widths: SidebarWidths) -> SidebarWidths {
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
        bookmarks: sanitize(widths.bookmarks, BOOKMARKS_SIDEBAR_WIDTH),
        notes: sanitize(widths.notes, NOTES_SIDEBAR_WIDTH),
        ai: sanitize(widths.ai, AI_PANEL_WIDTH),
    }
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

struct DocumentTranslationState {
    view_language: DocumentViewLanguage,
    view_role: TranslationViewRole,
    pair_active: bool,
    overlay_enabled: bool,
    position_sync_ready: bool,
    pending_viewport_anchor: Option<TranslationViewportAnchor>,
    suppress_linked_events: bool,
    external_hovered_region: Option<String>,
    hovered_region: Option<String>,
    popup: Option<TranslationCardState>,
    pinned: Option<TranslationCardState>,
    popup_scroll: ScrollHandle,
    pinned_scroll: ScrollHandle,
    popup_language_select: Option<TargetLanguageSelect>,
    pinned_language_select: Option<TargetLanguageSelect>,
    request_generation: u64,
    session_id: Option<DocumentTranslationId>,
    pair_session_id: Option<DocumentTranslationId>,
    session_snapshot: Option<DocumentTranslationSessionSnapshot>,
    hover_regions: HashMap<String, TranslationHoverRegionEntry>,
    failed_hover_regions: HashSet<String>,
    hover_animation: Option<TranslationHoverAnimation>,
    hover_clear_generation: u64,
    hover_clear_task: Option<Task<()>>,
    hover_tools_hovered: bool,
    hover_tools_bounds: Option<(String, Bounds<Pixels>)>,
    panel_placement: TranslationPanelPlacement,
    panel_drag: Option<TranslationPanelDrag>,
    panel_size: Size<Pixels>,
    panel_initial_dock_armed: bool,
    panel_initial_dock_generation: u64,
    panel_initial_dock_task: Option<Task<()>>,
    panel_dock_generation: u64,
    panel_dock_task: Option<Task<()>>,
    panel_retract_generation: u64,
    panel_retract_task: Option<Task<()>>,
    panel_dock_panel_hovered: bool,
    panel_dock_edge_hovered: bool,
}

impl DocumentTranslationState {
    fn new() -> Self {
        Self {
            view_language: DocumentViewLanguage::Original,
            view_role: TranslationViewRole::None,
            pair_active: false,
            overlay_enabled: true,
            position_sync_ready: false,
            pending_viewport_anchor: None,
            suppress_linked_events: false,
            external_hovered_region: None,
            hovered_region: None,
            popup: None,
            pinned: None,
            popup_scroll: ScrollHandle::new(),
            pinned_scroll: ScrollHandle::new(),
            popup_language_select: None,
            pinned_language_select: None,
            request_generation: 0,
            session_id: None,
            pair_session_id: None,
            session_snapshot: None,
            hover_regions: HashMap::new(),
            failed_hover_regions: HashSet::new(),
            hover_animation: None,
            hover_clear_generation: 0,
            hover_clear_task: None,
            hover_tools_hovered: false,
            hover_tools_bounds: None,
            panel_placement: TranslationPanelPlacement::Default,
            panel_drag: None,
            panel_size: size(
                px(TRANSLATION_PANEL_PREFERRED_WIDTH),
                px(TRANSLATION_PANEL_DEFAULT_HEIGHT),
            ),
            panel_initial_dock_armed: false,
            panel_initial_dock_generation: 0,
            panel_initial_dock_task: None,
            panel_dock_generation: 0,
            panel_dock_task: None,
            panel_retract_generation: 0,
            panel_retract_task: None,
            panel_dock_panel_hovered: false,
            panel_dock_edge_hovered: false,
        }
    }
}

#[derive(Clone, Copy)]
struct ZoomAnchor {
    document: [f32; 2],
    viewport: [f32; 2],
}

pub struct DocumentView {
    doc_id: u64,
    path: PathBuf,
    file_name: SharedString,
    format: DocumentFormat,
    password_protected: bool,
    capabilities: DocumentCapabilities,
    document_layout: DocumentLayout,
    epub_preferences: EpubReaderPreferences,
    document_colors: Option<DocumentColors>,
    epub_style_panel_position: Option<GpuiPoint<Pixels>>,
    document_snapshot: Arc<DocumentSnapshot>,
    render_session: DocumentRenderSession,
    gpu_pdf: gpu_render::GpuPdfState,
    page_render_sources: HashMap<usize, RevisionedPageRenderSource>,
    pages: Vec<DocumentPage>,
    page_count: usize,
    max_page_width: f32,
    content_height_doc: f32,
    glyphs: Vec<Glyph>,
    text_blocks: Vec<TextBlock>,
    text_lines: Vec<TextLine>,
    page_line_ranges: Vec<Range<usize>>,
    page_block_ranges: Vec<Range<usize>>,
    translation: DocumentTranslationState,
    links: Vec<DocumentLink>,
    highlights: Vec<Highlight>,
    notes: Vec<Note>,
    selected_highlight: Option<u64>,
    selection: Option<Selection>,
    region_tool_active: bool,
    region_drag: Option<RegionDragState>,
    selected_region: Option<RegionImageReference>,
    target_language: String,
    document_translation_jobs: Entity<DocumentTranslationManager>,
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
    viewport_render_debug: HashMap<usize, ViewportPageRenderDebug>,
    page_render_debug: HashMap<usize, PageRenderAttemptDebug>,
    rotation: DocumentRotation,
    page_layout: DocumentPageLayout,
    page_gap: f32,
    zoom: f32,
    target_zoom: f32,
    zoom_anchor: Option<ZoomAnchor>,
    last_animation_frame: Option<Instant>,
    zoom_popup_visible: bool,
    zoom_popup_hovered: bool,
    zoom_popup_generation: u64,
    zoom_popup_hide_task: Option<Task<()>>,
    base_scale: f32,
    viewport_origin: GpuiPoint<Pixels>,
    viewport_width: f32,
    viewport_height: f32,
    scroll_x_doc: f32,
    scroll_y_doc: f32,
    target_scroll_x_doc: f32,
    target_scroll_y_doc: f32,
    document_scroll: ManualScrollHandle,
    search_bar: Option<Entity<SearchBar>>,
    toc_palette: Option<Entity<TocPalette>>,
    notes_palette: Option<Entity<NotesPalette>>,
    history_palette: Option<Entity<HistoryPalette>>,
    bookmark_palette: Option<Entity<BookmarkPalette>>,
    bookmark_title_editor: Option<Entity<BookmarkTitleEditor>>,
    bookmark_title_edit: Option<BookmarkTitleEdit>,
    bookmarks: Vec<Bookmark>,
    bookmark_sidebar_scroll: UniformListScrollHandle,
    go_to_page_prompt: Option<Entity<GoToPagePrompt>>,
    note_editor: Option<Entity<NoteEditor>>,
    note_target: Option<NoteTarget>,
    toc: Vec<TocEntry>,
    toc_collapsed: HashSet<usize>,
    toc_sidebar_scroll: UniformListScrollHandle,
    toc_sidebar_visible_entry_ixs: Vec<usize>,
    toc_sidebar_entries_dirty: bool,
    toc_sidebar_selected: Option<usize>,
    toc_sidebar_follow_current: bool,
    toc_sidebar_manual_scroll: Rc<Cell<bool>>,
    toc_sidebar_last_scroll_offset: GpuiPoint<Pixels>,
    notes_sidebar_scroll: UniformListScrollHandle,
    notes_sidebar_entries: Vec<NotePaletteEntry>,
    notes_sidebar_selected: Option<u64>,
    thumbnail_sidebar_scroll: UniformListScrollHandle,
    thumbnail_sidebar_current_page: usize,
    thumbnail_sidebar_selecting: bool,
    thumbnail_sidebar_selection: BTreeSet<usize>,
    thumbnail_sidebar_selection_anchor: Option<usize>,
    viewport_render_keys: HashSet<RenderKey>,
    thumbnail_sidebar_render_keys: SurfaceRenderKeys,
    dock_snapshot: DocumentDockSnapshot,
    grid_overview_open: bool,
    grid_overview_scroll: UniformListScrollHandle,
    grid_overview_zoom: f32,
    grid_overview_cols: usize,
    grid_overview_cursor: usize,
    grid_overview_viewport_width: f32,
    grid_overview_selecting: bool,
    grid_overview_selection: BTreeSet<usize>,
    grid_overview_selection_anchor: Option<usize>,
    grid_overview_render_keys: SurfaceRenderKeys,
    presentation_page: Option<usize>,
    jump_back: Vec<JumpPosition>,
    jump_forward: Vec<JumpPosition>,
    position_from_search: bool,
    search: SearchState,
    pending_initial_position: Option<DocumentPosition>,
    pending_semantic_position: Option<DocumentPosition>,
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
    context_menu_popup: Option<Entity<PopupMenu>>,
    context_menu_page: Option<usize>,
    context_menu_image: Option<(usize, usize)>,
    context_menu_highlight: Option<u64>,
    context_menu_note: Option<u64>,
    context_menu_region: bool,
    document_edit_revision: DocumentEditRevision,
    bookmarks_revision: u64,
    _image_clipboard: Option<Clipboard>,
}

#[derive(Clone)]
pub struct DocumentViewAiContextProvider {
    document_view: gpui::WeakEntity<DocumentView>,
}

impl DocumentViewAiContextProvider {
    pub fn new(document_view: &Entity<DocumentView>) -> Self {
        Self {
            document_view: document_view.downgrade(),
        }
    }
}

struct DocumentAgentTools {
    snapshot: Arc<DocumentSnapshot>,
    document_layout: DocumentLayout,
    page_count: usize,
    current_page: usize,
    text_format: PageTextFormat,
    image_quality: PageImageQuality,
    page_image_limit: usize,
    rotation: DocumentRotation,
    reflow_text: Option<String>,
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

impl AiContextProvider for DocumentViewAiContextProvider {
    fn context(&self, include_document_text: bool, cx: &gpui::App) -> Option<AiContext> {
        self.document_view
            .read_with(cx, |document_view, _| AiContext {
                document_file_name: document_view
                    .path
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .map(str::to_string),
                document_format: Some(
                    match document_view.format {
                        DocumentFormat::Pdf => "PDF",
                        DocumentFormat::Epub => "EPUB",
                    }
                    .to_string(),
                ),
                location_label: document_view.capabilities.reflow.then(|| {
                    let progress = if document_view.max_scroll_y_doc() > 0.0 {
                        document_view.target_scroll_y_doc / document_view.max_scroll_y_doc()
                    } else {
                        0.0
                    };
                    format!(
                        "Reading progress: {}%",
                        format_user_float((progress.clamp(0.0, 1.0) * 100.0) as f64)
                    )
                }),
                page_count: if document_view.capabilities.reflow {
                    0
                } else {
                    document_view.page_count()
                },
                current_page: if document_view.capabilities.reflow {
                    0
                } else {
                    document_view.current_page()
                },
                selected_text: document_view.selected_text(),
                selected_region: document_view.selected_region.clone(),
                selected_pages: document_view.selected_pages_for_ai(),
                document_text: include_document_text
                    .then(|| document_view.document_text())
                    .flatten(),
            })
            .ok()
    }

    fn page_state(&self, cx: &gpui::App) -> Option<AiPageState> {
        self.document_view
            .read_with(cx, |document_view, _| document_view.ai_page_state())
            .ok()
    }

    fn document_scope(&self, cx: &gpui::App) -> Option<glyphst_ai::ai_chat::DocumentContextScope> {
        self.document_view
            .read_with(cx, |document_view, _| {
                glyphst_ai::ai_chat::DocumentContextScope {
                    path: document_view.path.clone(),
                    label: document_view.file_name.to_string(),
                }
            })
            .ok()
    }

    fn resolve_page_references(
        &self,
        references: Vec<MentionReference>,
        cx: &gpui::App,
    ) -> Task<Result<Vec<ResolvedPageContext>>> {
        let inputs = self
            .document_view
            .read_with(cx, |document_view, cx| {
                let ai_settings = &cx.global::<SettingsStore>().settings.ai;
                let references = DocumentView::prepare_page_references(
                    references,
                    document_view.document_layout,
                    document_view.page_count,
                )?;
                Ok((
                    document_view.document_snapshot.clone(),
                    document_view.document_layout,
                    document_view.page_count,
                    ai_settings.page_text_format,
                    ai_settings.page_image_quality,
                    document_view.rotation,
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
            DocumentView::resolve_page_contexts(
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
            .document_view
            .read_with(cx, |document_view, cx| {
                let store = cx.global::<SettingsStore>();
                Ok((
                    document_view.document_snapshot.clone(),
                    document_view.document_layout,
                    document_view.page_count,
                    store.settings.ai.page_image_quality,
                    store.settings.ai.page_image_limit,
                    document_view.rotation,
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
            DocumentView::resolve_region_contexts(
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

    fn agent_tools(&self, cx: &gpui::App) -> Option<Arc<dyn AgentToolExecutor>> {
        self.document_view
            .read_with(cx, |document_view, cx| {
                let ai_settings = &cx.global::<SettingsStore>().settings.ai;
                Arc::new(DocumentAgentTools {
                    snapshot: document_view.document_snapshot.clone(),
                    document_layout: document_view.document_layout,
                    page_count: document_view.page_count,
                    current_page: document_view.current_page(),
                    text_format: ai_settings.page_text_format,
                    image_quality: ai_settings.page_image_quality,
                    page_image_limit: ai_settings.page_image_limit,
                    rotation: document_view.rotation,
                    reflow_text: document_view
                        .capabilities
                        .reflow
                        .then(|| document_view.document_text())
                        .flatten(),
                }) as Arc<dyn AgentToolExecutor>
            })
            .ok()
    }
}

mod ai_context;
mod annotations;
mod bookmarks;
mod chrome;
mod context_menu;
mod controls;
mod destinations;
mod document;
mod epub;
mod export;
mod geometry;
mod gpu_render;
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
mod render_debug;
mod search_state;
mod selection;
mod sidebar;
mod text_index;
mod translation;
mod viewport;

pub use render_debug::render_cache_debug_snapshot;

impl EventEmitter<DocumentViewEvent> for DocumentView {}

#[cfg(test)]
mod tests;
