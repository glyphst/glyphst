use std::{
    borrow::Cow,
    cell::RefCell,
    collections::HashSet,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use crate::bookmarks::{
    BookmarkPalette, BookmarkPaletteDelegate, BookmarkPrompt, BookmarkPromptDelegate,
};
use crate::command_palette::{CommandPalette, CommandPaletteDelegate};
use crate::history::{HistoryPalette, HistoryPaletteDelegate};
use crate::keybindings;
use crate::library::{
    DocumentPosition, Highlight, HighlightColor, HighlightRect, LibraryStore, Note, normalize_path,
};
use crate::note_editor::{NoteEditor, NoteEditorEvent};
use crate::notes::{NotePaletteEntry, NotesPalette, NotesPaletteDelegate};
use crate::picker::{Picker, PickerEvent};
use crate::search::{SearchBar, SearchEvent};
use crate::settings::{BehaviorSettings, SettingsStore, WindowSettings};
use crate::settings_panel::{PanelEvent, SettingsPanel};
use crate::smooth_scroll::advance_toward;
use crate::theme::Theme;
use crate::toc::{TocPalette, TocPaletteDelegate, TocPaletteEntry};
use anyhow::{Context as _, Result, anyhow, bail};
use arboard::{Clipboard, ImageData};
use gpui::{
    Anchor, Bounds, ClipboardItem, Context, Corners, Entity, FocusHandle, Focusable, IntoElement,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point as GpuiPoint, Render, RenderImage, ScrollDelta, ScrollWheelEvent, SharedString, Task,
    UniformListScrollHandle, Window, anchored, canvas, deferred, div, fill, point, prelude::*, px,
    rgb, rgba, size, uniform_list,
};
use image::{Frame, RgbaImage};
use mupdf::text_page::TextBlockType;
use mupdf::{
    ColorParams, Colorspace, DestinationKind, Device, DisplayList, Image as PdfImage, Matrix,
    Pixmap, Quad, Rect, TextPageFlags,
};
use smallvec::smallvec;

gpui::actions!(
    pdf_viewer,
    [
        CopySelection,
        SelectAll,
        ZoomIn,
        ZoomOut,
        ResetZoom,
        Search,
        SearchNext,
        SearchPrev,
        NextPage,
        PreviousPage,
        NextHalfPage,
        PreviousHalfPage,
        JumpBackward,
        JumpForward,
        Toc,
        ToggleToc,
        OpenNotes,
        ToggleNotes,
        ToggleCommandPalette,
        ToggleFullScreen,
        ToggleTransparency,
        OpenSettings,
        OpenHistory,
        OpenBookmarks,
        AddBookmark
    ]
);

const INITIAL_SELECTION_TOLERANCE_PX: f32 = 24.0;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 8.0;
const SCROLL_LINE_MULTIPLIER: f32 = 20.0;
const CTRL_SCROLL_ZOOM_STEP_MULTIPLIER: f32 = 0.25;
const MAX_CACHED_RENDERS: usize = 12;
const MAX_JUMP_HISTORY: usize = 128;
const JUMP_POSITION_EPSILON_DOC: f32 = 0.5;
const TOC_SIDEBAR_WIDTH: f32 = 280.0;
const TOC_SIDEBAR_ROW_HEIGHT: f32 = 30.0;
const NOTES_SIDEBAR_WIDTH: f32 = 300.0;
const NOTES_SIDEBAR_ROW_HEIGHT: f32 = 52.0;
const SIDEBAR_PAGE_NUMBER_WIDTH: f32 = 38.0;
const INFERRED_SPACE_WIDTH_RATIO: f32 = 0.75;
const INFERRED_SPACE_HEIGHT_RATIO: f32 = 0.20;

struct PdfPage {
    display_list: Arc<DisplayList>,
    bounds: Rect,
    width: f32,
    height: f32,
    offset_y: f32,
    glyph_range: Range<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct RenderKey {
    page_idx: usize,
    physical_width: u32,
    physical_height: u32,
}

struct CachedRender {
    key: RenderKey,
    render_scale: f32,
    last_access: u64,
    image: Arc<RenderImage>,
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

struct PaintData {
    pages: Vec<PaintPage>,
    highlight_overlays: Vec<HighlightOverlay>,
    selected_highlight_borders: Vec<Bounds<Pixels>>,
    note_underlines: Vec<(Bounds<Pixels>, HighlightColor)>,
    link_highlights: Vec<Bounds<Pixels>>,
    selection_highlights: Vec<Bounds<Pixels>>,
    search_highlights: Vec<Bounds<Pixels>>,
    active_search_highlights: Vec<Bounds<Pixels>>,
}

#[derive(Clone)]
struct Glyph {
    c: char,
    quad: Quad,
    page_idx: usize,
    line_idx: u32,
    block_idx: u32,
}

#[derive(Clone, Copy)]
struct Selection {
    anchor: usize,
    head: usize,
}

#[derive(Clone, Copy)]
enum HighlightColorAction {
    Create,
    Change,
}

enum NoteTarget {
    New {
        rects: Vec<HighlightRect>,
        text: String,
    },
    Edit {
        id: u64,
    },
}

pub(crate) struct PdfAnnotations {
    pub(crate) highlights: Vec<Highlight>,
    pub(crate) notes: Vec<Note>,
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
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: "".into(),
            matches: Vec::new(),
            active: None,
            ignore_case: true,
        }
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

pub struct PdfView {
    path: PathBuf,
    file_name: SharedString,
    pages: Vec<PdfPage>,
    max_page_width: f32,
    content_height_doc: f32,
    glyphs: Vec<Glyph>,
    links: Vec<PdfLink>,
    highlights: Vec<Highlight>,
    notes: Vec<Note>,
    selected_highlight: Option<u64>,
    selection: Option<Selection>,
    dragging: bool,
    hovered_link: Option<usize>,
    hovered_note: Option<u64>,
    hovered_note_position: Option<GpuiPoint<Pixels>>,
    pending_link: Option<usize>,
    panning: Option<PanState>,
    page_image_bounds: Vec<Option<Bounds<Pixels>>>,
    focus_handle: FocusHandle,
    cached: Vec<CachedRender>,
    cache_access_counter: u64,
    pending_renders: Vec<PendingRender>,
    control_down: bool,
    zoom: f32,
    base_scale: f32,
    viewport_width: f32,
    viewport_height: f32,
    scroll_x_doc: f32,
    scroll_y_doc: f32,
    target_scroll_x_doc: f32,
    target_scroll_y_doc: f32,
    command_palette: Option<Entity<CommandPalette>>,
    settings_panel: Option<Entity<SettingsPanel>>,
    search_bar: Option<Entity<SearchBar>>,
    toc_palette: Option<Entity<TocPalette>>,
    notes_palette: Option<Entity<NotesPalette>>,
    history_palette: Option<Entity<HistoryPalette>>,
    bookmark_palette: Option<Entity<BookmarkPalette>>,
    bookmark_prompt: Option<Entity<BookmarkPrompt>>,
    note_editor: Option<Entity<NoteEditor>>,
    note_target: Option<NoteTarget>,
    toc: Vec<TocEntry>,
    toc_collapsed: HashSet<usize>,
    toc_sidebar_open: bool,
    toc_sidebar_scroll: UniformListScrollHandle,
    toc_sidebar_visible_entry_ixs: Vec<usize>,
    toc_sidebar_entries_dirty: bool,
    notes_sidebar_open: bool,
    notes_sidebar_scroll: UniformListScrollHandle,
    notes_sidebar_entries: Vec<NotePaletteEntry>,
    notes_sidebar_selected: Option<u64>,
    jump_back: Vec<JumpPosition>,
    jump_forward: Vec<JumpPosition>,
    position_from_search: bool,
    transparency_enabled: bool,
    window_settings: WindowSettings,
    search: SearchState,
    position_save_task: Option<Task<()>>,
    context_menu_position: Option<GpuiPoint<Pixels>>,
    context_menu_image: Option<(usize, usize)>,
    context_menu_highlight: Option<u64>,
    context_menu_note: Option<u64>,
    _image_clipboard: Option<Clipboard>,
    // Must drop after pages: Type3 fonts in display lists reference the document.
    _document: mupdf::Document,
}

impl PdfView {
    pub fn open(
        path: impl AsRef<Path>,
        focus_handle: FocusHandle,
        behavior: BehaviorSettings,
        window_settings: WindowSettings,
        theme: Theme,
        initial_position: Option<DocumentPosition>,
        annotations: PdfAnnotations,
    ) -> Result<Self> {
        let path = normalize_path(path.as_ref());
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Untitled")
            .to_string()
            .into();

        let document = mupdf::Document::open(
            path.to_str()
                .ok_or_else(|| anyhow!("PDF path is not valid UTF-8"))?,
        )?;
        let page_count = document.page_count()?;
        if page_count == 0 {
            bail!("PDF has no pages");
        }
        let outlines = match document.outlines() {
            Ok(outlines) => outlines,
            Err(error) => {
                eprintln!("failed to read PDF table of contents: {error:#}");
                Vec::new()
            }
        };
        let toc = Self::flatten_toc(outlines, page_count as usize);

        let mut pages = Vec::with_capacity(page_count as usize);
        let mut glyphs = Vec::new();
        let mut links = Vec::new();
        let mut offset_y = 0.0;
        let mut max_page_width = 0.0_f32;
        let mut next_line_idx = 0;
        let mut next_block_idx = 0;

        for page_no in 0..page_count {
            let page_idx = page_no as usize;
            let page = document.load_page(page_no)?;
            let page_bounds = page.bounds()?;
            let page_width = page_bounds.x1 - page_bounds.x0;
            let page_height = page_bounds.y1 - page_bounds.y0;

            let display_list = Arc::new(DisplayList::new(page_bounds)?);
            let device = Device::from_display_list(&display_list)?;
            page.run(&device, &Matrix::IDENTITY)?;

            let explicit_links_start = links.len();
            match Self::extract_explicit_links(&page, page_idx, page_count as usize) {
                Ok(page_links) => links.extend(page_links),
                Err(error) => eprintln!(
                    "failed to read PDF links on page {}: {error:#}",
                    page_idx + 1
                ),
            }

            let glyph_range = Self::extract_glyphs(
                &display_list,
                page_idx,
                &mut glyphs,
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

            pages.push(PdfPage {
                display_list,
                bounds: page_bounds,
                width: page_width,
                height: page_height,
                offset_y,
                glyph_range,
            });

            max_page_width = max_page_width.max(page_width);
            offset_y += page_height + theme.page_gap;
        }

        let content_height_doc = offset_y - theme.page_gap;
        let page_image_bounds = (0..pages.len()).map(|_| None).collect();

        let mut view = Self {
            path,
            file_name,
            pages,
            max_page_width,
            content_height_doc,
            glyphs,
            links,
            highlights: annotations.highlights,
            notes: annotations.notes,
            selected_highlight: None,
            selection: None,
            dragging: false,
            hovered_link: None,
            hovered_note: None,
            hovered_note_position: None,
            pending_link: None,
            panning: None,
            page_image_bounds,
            focus_handle,
            cached: Vec::new(),
            cache_access_counter: 0,
            pending_renders: Vec::new(),
            control_down: false,
            zoom: behavior.default_zoom,
            base_scale: 1.0,
            viewport_width: 0.0,
            viewport_height: 0.0,
            scroll_x_doc: 0.0,
            scroll_y_doc: 0.0,
            target_scroll_x_doc: 0.0,
            target_scroll_y_doc: 0.0,
            command_palette: None,
            settings_panel: None,
            search_bar: None,
            toc_palette: None,
            notes_palette: None,
            history_palette: None,
            bookmark_palette: None,
            bookmark_prompt: None,
            note_editor: None,
            note_target: None,
            toc,
            toc_collapsed: HashSet::new(),
            toc_sidebar_open: false,
            toc_sidebar_scroll: UniformListScrollHandle::new(),
            toc_sidebar_visible_entry_ixs: Vec::new(),
            toc_sidebar_entries_dirty: true,
            notes_sidebar_open: false,
            notes_sidebar_scroll: UniformListScrollHandle::new(),
            notes_sidebar_entries: Vec::new(),
            notes_sidebar_selected: None,
            jump_back: Vec::new(),
            jump_forward: Vec::new(),
            position_from_search: false,
            transparency_enabled: window_settings.transparency_enabled_by_default(),
            window_settings,
            search: SearchState::default(),
            position_save_task: None,
            context_menu_position: None,
            context_menu_image: None,
            context_menu_highlight: None,
            context_menu_note: None,
            _image_clipboard: None,
            _document: document,
        };

        if let Some(position) = initial_position {
            view.restore_position(position);
        }

        Ok(view)
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

    fn extract_glyphs(
        display_list: &DisplayList,
        page_idx: usize,
        glyphs: &mut Vec<Glyph>,
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
                for ch in line.chars() {
                    if let Some(c) = ch.char() {
                        glyphs.push(Glyph {
                            c,
                            quad: ch.quad(),
                            page_idx,
                            line_idx,
                            block_idx,
                        });
                    }
                }

                *next_line_idx = next_line_idx.saturating_add(1);
            }
        }

        Ok(start..glyphs.len())
    }

    fn paint_data(
        &mut self,
        bounds: Bounds<Pixels>,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> Result<PaintData> {
        let viewport_width = bounds.size.width.as_f32().max(1.0);
        self.viewport_width = viewport_width;
        self.viewport_height = bounds.size.height.as_f32().max(1.0);
        self.base_scale = viewport_width / self.max_page_width.max(1.0);
        self.clamp_scroll();

        self.page_image_bounds.clear();
        self.page_image_bounds
            .resize_with(self.pages.len(), || None);

        let scale = self.effective_scale();
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

        if let (Some(first), Some(last)) = (visible_pages.first(), visible_pages.last()) {
            let start = first.saturating_sub(1);
            let end = (*last + 1).min(self.pages.len().saturating_sub(1));
            for page_idx in start..=end {
                if page_idx >= *first && page_idx <= *last {
                    continue;
                }

                let (_, key) =
                    self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
                self.request_render(key, cx);
            }
        }

        let mut pages = Vec::with_capacity(visible_pages.len());
        for page_idx in visible_pages {
            let (image_bounds, key) =
                self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
            let image = self.cached_or_request_render(key, cx);
            pages.push(PaintPage {
                bounds: image_bounds,
                image,
            });
        }

        let (highlight_overlays, selected_highlight_borders) = self.highlight_overlays();
        let note_underlines = self.note_underlines();
        let selection_highlights = self.selection_highlights();
        let (search_highlights, active_search_highlights) = self.search_highlights();
        let link_highlights = self.link_highlights();

        Ok(PaintData {
            pages,
            highlight_overlays,
            selected_highlight_borders,
            note_underlines,
            link_highlights,
            selection_highlights,
            search_highlights,
            active_search_highlights,
        })
    }

    fn image_bounds_for_page(
        &self,
        page_idx: usize,
        bounds: Bounds<Pixels>,
        scale: f32,
    ) -> Bounds<Pixels> {
        let page = &self.pages[page_idx];
        let image_width = px(page.width * scale);
        let image_height = px(page.height * scale);
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
        let key = Self::render_key(page_idx, image_bounds, scale_factor);
        (
            Self::pixel_aligned_image_bounds(image_bounds, key, scale_factor),
            key,
        )
    }

    fn render_key(page_idx: usize, image_bounds: Bounds<Pixels>, scale_factor: f32) -> RenderKey {
        let scale_factor = scale_factor.max(0.001);
        RenderKey {
            page_idx,
            physical_width: (image_bounds.size.width.as_f32() * scale_factor)
                .ceil()
                .max(1.0) as u32,
            physical_height: (image_bounds.size.height.as_f32() * scale_factor)
                .ceil()
                .max(1.0) as u32,
        }
    }

    fn pixel_aligned_image_bounds(
        image_bounds: Bounds<Pixels>,
        key: RenderKey,
        scale_factor: f32,
    ) -> Bounds<Pixels> {
        let scale_factor = scale_factor.max(0.001);
        Bounds::new(
            image_bounds.origin,
            size(
                px(key.physical_width as f32 / scale_factor),
                px(key.physical_height as f32 / scale_factor),
            ),
        )
    }

    fn effective_scale(&self) -> f32 {
        (self.base_scale * self.zoom).max(0.001)
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
        for page in &mut self.pages {
            page.offset_y = offset_y;
            offset_y += page.height + page_gap;
        }

        self.content_height_doc = offset_y - page_gap;
        self.clamp_scroll();
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
        if !smooth_scrolling {
            self.scroll_x_doc = self.target_scroll_x_doc;
            self.scroll_y_doc = self.target_scroll_y_doc;
        }

        let changed = (self.target_scroll_x_doc - previous_x).abs() > f32::EPSILON
            || (self.target_scroll_y_doc - previous_y).abs() > f32::EPSILON;
        if changed {
            self.hovered_link = None;
            self.pending_link = None;
            self.schedule_position_save(cx);
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

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn restore_position(&mut self, position: DocumentPosition) {
        self.zoom = position.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.scroll_x_doc = position.scroll_x_doc;
        self.scroll_y_doc = position.scroll_y_doc;
        self.target_scroll_x_doc = position.scroll_x_doc;
        self.target_scroll_y_doc = position.scroll_y_doc;
        self.clamp_scroll();
    }

    fn current_document_position(&self) -> DocumentPosition {
        DocumentPosition {
            scroll_x_doc: self.target_scroll_x_doc,
            scroll_y_doc: self.target_scroll_y_doc,
            zoom: self.zoom,
            page: self.page_index_for_scroll_y(self.target_scroll_y_doc) + 1,
        }
    }

    fn record_current_history_position(&self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let position = self.current_document_position();
        cx.global_mut::<LibraryStore>()
            .record_history(&path, position);
    }

    fn flush_current_history_position(&mut self, cx: &mut Context<Self>) {
        self.position_save_task.take();
        self.record_current_history_position(cx);
        if let Err(error) = cx.global::<LibraryStore>().save() {
            eprintln!("failed to save PDF history: {error:#}");
        }
    }

    fn schedule_position_save(&mut self, cx: &mut Context<Self>) {
        self.record_current_history_position(cx);
        self.position_save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1000))
                .await;
            _ = this.update(cx, |_, cx| {
                if let Err(error) = cx.global::<LibraryStore>().save() {
                    eprintln!("failed to save PDF history: {error:#}");
                }
            });
        }));
    }

    fn current_jump_position(&self) -> JumpPosition {
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
    }

    fn jump_backward(&mut self, cx: &mut Context<Self>) {
        let Some(position) = self.jump_back.pop() else {
            return;
        };

        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_forward, current_position);
        self.set_scroll_doc(position.scroll_x_doc, position.scroll_y_doc, cx);
        self.mark_manual_navigation();
    }

    fn jump_forward(&mut self, cx: &mut Context<Self>) {
        let Some(position) = self.jump_forward.pop() else {
            return;
        };

        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        self.set_scroll_doc(position.scroll_x_doc, position.scroll_y_doc, cx);
        self.mark_manual_navigation();
    }

    fn request_render(&mut self, key: RenderKey, cx: &mut Context<Self>) {
        if self.exact_cached_render_index(key).is_none() {
            self.start_background_render(key, cx);
        }
    }

    fn cached_or_request_render(
        &mut self,
        key: RenderKey,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        if let Some(index) = self.exact_cached_render_index(key) {
            return Some(self.touch_cached_render(index));
        }

        self.start_background_render(key, cx);
        self.scaled_fallback_render_index(key)
            .map(|index| self.touch_cached_render(index))
    }

    fn exact_cached_render_index(&self, key: RenderKey) -> Option<usize> {
        self.cached.iter().position(|cached| cached.key == key)
    }

    fn scaled_fallback_render_index(&self, key: RenderKey) -> Option<usize> {
        let render_scale = key.physical_width as f32 / self.pages[key.page_idx].width;
        self.cached
            .iter()
            .enumerate()
            .filter(|(_, cached)| {
                cached.key.page_idx == key.page_idx
                    && cached.key.physical_width <= key.physical_width
                    && cached.key.physical_height <= key.physical_height
            })
            .min_by(|(_, a), (_, b)| {
                (a.render_scale - render_scale)
                    .abs()
                    .total_cmp(&(b.render_scale - render_scale).abs())
            })
            .map(|(index, _)| index)
    }

    fn touch_cached_render(&mut self, index: usize) -> Arc<RenderImage> {
        let last_access = self.next_cache_access();
        self.cached[index].last_access = last_access;
        self.cached[index].image.clone()
    }

    fn insert_cached_render(&mut self, key: RenderKey, image: Arc<RenderImage>) {
        let last_access = self.next_cache_access();
        let render_scale = key.physical_width as f32 / self.pages[key.page_idx].width;

        if let Some(index) = self.exact_cached_render_index(key) {
            self.cached[index] = CachedRender {
                key,
                render_scale,
                last_access,
                image,
            };
        } else {
            self.cached.push(CachedRender {
                key,
                render_scale,
                last_access,
                image,
            });
        }

        while self.cached.len() > MAX_CACHED_RENDERS {
            if let Some(index) = self
                .cached
                .iter()
                .enumerate()
                .min_by_key(|(_, cached)| cached.last_access)
                .map(|(index, _)| index)
            {
                self.cached.remove(index);
            }
        }
    }

    fn next_cache_access(&mut self) -> u64 {
        self.cache_access_counter = self.cache_access_counter.wrapping_add(1);
        self.cache_access_counter
    }

    fn start_background_render(&mut self, key: RenderKey, cx: &mut Context<Self>) {
        if self
            .pending_renders
            .iter()
            .any(|pending| pending.key == key)
        {
            return;
        }
        self.pending_renders
            .retain(|pending| pending.key.page_idx != key.page_idx);

        let page = &self.pages[key.page_idx];
        let display_list = page.display_list.clone();
        let page_bounds = page.bounds;
        let page_width = page.width;
        let page_height = page.height;

        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    Self::render_page(
                        &display_list,
                        page_bounds,
                        page_width,
                        page_height,
                        key.physical_width,
                        key.physical_height,
                    )
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
                        Ok(image) => {
                            this.insert_cached_render(key, image);
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

    fn render_page(
        display_list: &DisplayList,
        page_bounds: Rect,
        page_width: f32,
        page_height: f32,
        physical_width: u32,
        physical_height: u32,
    ) -> Result<Arc<RenderImage>> {
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

        let matrix = Matrix::new(
            zoom_x,
            0.0,
            0.0,
            zoom_y,
            -page_bounds.x0 * zoom_x,
            -page_bounds.y0 * zoom_y,
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

    fn collect_images(display_list: &DisplayList) -> Result<Vec<CollectedImage>> {
        let collector = Rc::new(RefCell::new(ImageCollector::default()));
        let device = Device::from_native(collector.clone())?;
        display_list
            .run(&device, &Matrix::IDENTITY, Rect::INF)
            .context("failed to collect PDF images")?;
        drop(device);

        Ok(std::mem::take(&mut collector.borrow_mut().images))
    }

    fn image_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<(usize, usize)> {
        let page_idx = self.page_at_screen(point)?;
        let pdf_point = self.screen_to_pdf(page_idx, point)?;
        let images = match Self::collect_images(&self.pages[page_idx].display_list) {
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
        let Some(page) = self.pages.get(page_idx) else {
            return;
        };
        let display_list = page.display_list.clone();

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
        let Some(page) = self.pages.get(page_idx) else {
            return;
        };
        let display_list = page.display_list.clone();
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
        let scale_x = bounds.size.width.as_f32() / page.width;
        let scale_y = bounds.size.height.as_f32() / page.height;
        if scale_x <= 0.0 || scale_y <= 0.0 {
            return None;
        }

        Some(mupdf::Point {
            x: (point.x.as_f32() - bounds.origin.x.as_f32()) / scale_x + page.bounds.x0,
            y: (point.y.as_f32() - bounds.origin.y.as_f32()) / scale_y + page.bounds.y0,
        })
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

        if !Self::same_word_segment(left, right)
            || left.c.is_whitespace()
            || right.c.is_whitespace()
            || left.c.is_control()
            || right.c.is_control()
        {
            return false;
        }

        let left_rect = Self::quad_rect(&left.quad);
        let right_rect = Self::quad_rect(&right.quad);
        let gap = right_rect.x0 - left_rect.x1;
        if gap <= 0.0 {
            return false;
        }

        gap >= Self::inferred_space_threshold(glyphs, left_ix)
    }

    fn inferred_space_threshold(glyphs: &[Glyph], ix: usize) -> f32 {
        let Some(glyph) = glyphs.get(ix) else {
            return 0.0;
        };
        let mut space_widths = glyphs
            .iter()
            .filter(|candidate| Self::same_word_segment(candidate, glyph))
            .filter(|candidate| candidate.c.is_whitespace() && !candidate.c.is_control())
            .filter_map(|candidate| {
                let rect = Self::quad_rect(&candidate.quad);
                let width = rect.x1 - rect.x0;
                (width > 0.0).then_some(width)
            })
            .collect::<Vec<_>>();

        if !space_widths.is_empty() {
            space_widths.sort_by(|a, b| a.total_cmp(b));
            return space_widths[space_widths.len() / 2] * INFERRED_SPACE_WIDTH_RATIO;
        }

        let rect = Self::quad_rect(&glyph.quad);
        (rect.y1 - rect.y0) * INFERRED_SPACE_HEIGHT_RATIO
    }

    fn nearby_glyph_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        let page_idx = self.page_at_screen(point)?;
        let bounds = self.page_image_bounds.get(page_idx)?.as_ref()?;
        let page = &self.pages[page_idx];
        let scale = (bounds.size.width.as_f32() / page.width)
            .min(bounds.size.height.as_f32() / page.height);
        if scale <= 0.0 {
            return None;
        }

        let point = self.screen_to_pdf(page_idx, point)?;
        self.nearby_glyph_at_pdf(page_idx, point, INITIAL_SELECTION_TOLERANCE_PX / scale)
    }

    fn exact_glyph_at_pdf(&self, page_idx: usize, point: mupdf::Point) -> Option<usize> {
        let range = self.pages[page_idx].glyph_range.clone();
        self.glyphs[range.clone()]
            .iter()
            .enumerate()
            .find_map(|(offset, glyph)| {
                Self::quad_rect(&glyph.quad)
                    .contains(point.x, point.y)
                    .then_some(range.start + offset)
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

        let range = self.pages[page_idx].glyph_range.clone();
        self.glyphs[range.clone()]
            .iter()
            .enumerate()
            .filter_map(|(offset, glyph)| {
                let distance = Self::distance_to_rect(point, Self::quad_rect(&glyph.quad));
                (distance <= tolerance).then_some((range.start + offset, distance))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(ix, _)| ix)
    }

    fn glyph_at_pdf(&self, page_idx: usize, point: mupdf::Point) -> Option<usize> {
        if self.pages[page_idx].glyph_range.is_empty() {
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
        let range = self.pages[page_idx].glyph_range.clone();
        let mut best = None;
        let mut start = range.start;

        while start < range.end {
            let line_idx = self.glyphs[start].line_idx;
            let mut end = start;
            let mut rect = Self::quad_rect(&self.glyphs[start].quad);

            while end + 1 < range.end && self.glyphs[end + 1].line_idx == line_idx {
                end += 1;
                rect = Self::union_rect(rect, Self::quad_rect(&self.glyphs[end].quad));
            }

            let dy = if point.y < rect.y0 {
                rect.y0 - point.y
            } else if point.y > rect.y1 {
                point.y - rect.y1
            } else {
                0.0
            };

            if best.as_ref().is_none_or(|(best_dy, _, _, _)| dy < *best_dy) {
                best = Some((dy, start, end, rect));
            }

            start = end + 1;
        }

        best
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

    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_bounds()?;
        Self::text_for_glyph_range(&self.glyphs, start..end + 1)
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

    fn copy_selection(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
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

        match cx
            .global_mut::<LibraryStore>()
            .add_highlight(&self.path, color, rects, text, "")
        {
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

        match cx
            .global_mut::<LibraryStore>()
            .set_highlight_color(id, color)
        {
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

        match cx.global_mut::<LibraryStore>().remove_highlight(id) {
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
            NoteTarget::New { rects, text },
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
            NoteTarget::New { rects, text } => {
                match cx
                    .global_mut::<LibraryStore>()
                    .add_note(&self.path, color, rects, text, body)
                {
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
                match cx
                    .global_mut::<LibraryStore>()
                    .update_note(id, body.clone(), color)
                {
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

        match cx.global_mut::<LibraryStore>().remove_note(id) {
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
            self.navigate_search(previous, cx);
            return;
        }

        self.search.query = query;
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
        if needle.is_empty() {
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

        for glyph_ix in self.pages[page_idx].glyph_range.clone() {
            let glyph = &self.glyphs[glyph_ix];
            if let Some(previous) = previous {
                if glyph.block_idx != previous.block_idx {
                    Self::push_search_separator(&mut text, &mut glyph_map);
                    Self::push_search_separator(&mut text, &mut glyph_map);
                } else if glyph.line_idx != previous.line_idx {
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
            search_bar.update(cx, |search_bar, cx| {
                search_bar.set_match_status(query.clone(), active, match_count, ignore_case, cx);
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
        cx.notify();
    }

    fn zoom_in(&mut self, cx: &mut Context<Self>) {
        let zoom_step = cx.global::<SettingsStore>().settings.behavior.zoom_step;
        self.set_zoom(self.zoom * zoom_step, cx);
    }

    fn zoom_out(&mut self, cx: &mut Context<Self>) {
        let zoom_step = cx.global::<SettingsStore>().settings.behavior.zoom_step;
        self.set_zoom(self.zoom / zoom_step, cx);
    }

    fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        let default_zoom = cx.global::<SettingsStore>().settings.behavior.default_zoom;
        self.set_zoom(default_zoom, cx);
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

    fn current_page_index(&self) -> usize {
        self.page_index_for_scroll_y(self.scroll_y_doc)
    }

    fn current_page(&self) -> usize {
        self.current_page_index() + 1
    }

    fn next_page(&mut self, cx: &mut Context<Self>) {
        let target_page = (self.current_page_index() + 1).min(self.pages.len().saturating_sub(1));
        let target = self
            .pages
            .get(target_page)
            .map(|page| page.offset_y)
            .unwrap_or_else(|| self.max_scroll_y_doc());

        if self.set_scroll_doc_target(self.target_scroll_x_doc, target, cx) {
            self.mark_manual_navigation();
        }
    }

    fn previous_page(&mut self, cx: &mut Context<Self>) {
        let target_page = self.current_page_index().saturating_sub(1);
        let target = self
            .pages
            .get(target_page)
            .map(|page| page.offset_y)
            .unwrap_or(0.0);

        if self.set_scroll_doc_target(self.target_scroll_x_doc, target, cx) {
            self.mark_manual_navigation();
        }
    }

    fn next_half_page(&mut self, cx: &mut Context<Self>) {
        self.scroll_half_page(1.0, cx);
    }

    fn previous_half_page(&mut self, cx: &mut Context<Self>) {
        self.scroll_half_page(-1.0, cx);
    }

    fn scroll_half_page(&mut self, direction: f32, cx: &mut Context<Self>) {
        let delta = self.viewport_height / (2.0 * self.effective_scale());
        if self.set_scroll_doc_target(
            self.target_scroll_x_doc,
            self.target_scroll_y_doc + direction * delta,
            cx,
        ) {
            self.mark_manual_navigation();
        }
    }

    fn handle_scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if self.control_down {
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
            self.set_zoom(self.zoom * factor, cx);
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

    fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_some() {
            self.close_command_palette(window, cx);
            return;
        }
        self.close_all_overlays(window, cx);

        let actions = window
            .available_actions(cx)
            .into_iter()
            .filter(|action| !action.partial_eq(&ToggleCommandPalette))
            .collect::<Vec<_>>();
        let previous_focus_handle = self.focus_handle.clone();
        let delegate = CommandPaletteDelegate::new(actions);
        let palette = cx.new(|cx| Picker::new(previous_focus_handle.clone(), delegate, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_command_palette(window, cx);
        })
        .detach();

        palette.focus_handle(cx).focus(window, cx);
        self.command_palette = Some(palette);
        cx.notify();
    }

    fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn close_all_overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_command_palette(window, cx);
        self.close_settings_panel(window, cx);
        self.close_search_bar(window, cx);
        self.close_toc_palette(window, cx);
        self.close_notes_palette(window, cx);
        self.close_history_palette(window, cx);
        self.close_bookmark_palette(window, cx);
        self.close_bookmark_prompt(window, cx);
    }

    fn toggle_settings_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_panel.is_some() {
            self.close_settings_panel(window, cx);
            return;
        }
        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let panel = cx.new(|cx| SettingsPanel::new(previous_focus_handle.clone(), cx));

        cx.subscribe_in(
            &panel,
            window,
            |this, _, event: &PanelEvent, window, cx| match event {
                PanelEvent::Dismissed => this.close_settings_panel(window, cx),
                PanelEvent::Changed => {
                    let (page_gap, window_settings, keymap_preset, keybinding_overrides) = {
                        let store = cx.global::<SettingsStore>();
                        (
                            store.resolved_theme.page_gap,
                            store.settings.window,
                            store.settings.keymap_preset,
                            store.settings.keybinding_overrides.clone(),
                        )
                    };
                    keybindings::apply(cx, keymap_preset, &keybinding_overrides);
                    this.recompute_layout(page_gap);
                    if this.window_settings != window_settings {
                        this.transparency_enabled =
                            window_settings.transparency_enabled_by_default();
                    }
                    this.window_settings = window_settings;
                    this.apply_transparency(window, cx);
                }
            },
        )
        .detach();

        panel.focus_handle(cx).focus(window, cx);
        self.settings_panel = Some(panel);
        cx.notify();
    }

    fn apply_transparency(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = cx.global::<SettingsStore>().settings.window;
        window.set_background_appearance(settings.background_appearance(self.transparency_enabled));
        cx.notify();
    }

    fn toggle_transparency(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.transparency_enabled = !self.transparency_enabled;
        self.apply_transparency(window, cx);
    }

    fn close_settings_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_panel.take().is_some() {
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
        let search_bar = cx.new(|cx| {
            SearchBar::new(
                previous_focus_handle.clone(),
                query.clone(),
                active,
                match_count,
                ignore_case,
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
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn toggle_toc_sidebar(&mut self, cx: &mut Context<Self>) {
        if self.toc.is_empty() {
            return;
        }

        self.toc_sidebar_open = !self.toc_sidebar_open;
        cx.notify();
    }

    fn toggle_toc_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.toc_palette.is_some() {
            self.close_toc_palette(window, cx);
            return;
        }

        let entries = self.toc_palette_entries();
        if entries.is_empty() {
            return;
        }

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

    fn toggle_notes_sidebar(&mut self, cx: &mut Context<Self>) {
        if !self.has_navigable_notes() {
            return;
        }

        if !self.notes_sidebar_open {
            self.refresh_notes_sidebar_entries();
        }

        self.notes_sidebar_open = !self.notes_sidebar_open;
        cx.notify();
    }

    fn toggle_notes_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notes_palette.is_some() {
            self.close_notes_palette(window, cx);
            return;
        }

        let entries = self.note_palette_entries();
        if entries.is_empty() {
            return;
        }

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

    fn has_navigable_notes(&self) -> bool {
        self.notes.iter().any(|note| !note.rects.is_empty())
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

    fn default_bookmark_label(&self) -> SharedString {
        let current_page = self.current_page_index();
        self.toc
            .iter()
            .rfind(|entry| entry.navigable && entry.page_idx <= current_page)
            .map(|entry| entry.title.clone())
            .unwrap_or_else(|| format!("Page {}", self.current_page()).into())
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

        let scroll_y_doc = target_top
            .map(|top| page.offset_y + top - page.bounds.y0)
            .unwrap_or(page.offset_y);
        let scroll_x_doc = target_left
            .map(|left| left - page.bounds.x0 - page.width / 2.0)
            .unwrap_or(self.scroll_x_doc);

        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        self.jump_forward.clear();
        self.position_from_search = false;
        self.set_scroll_doc(scroll_x_doc, scroll_y_doc, cx);
    }

    pub(crate) fn navigate_to_document_position(
        &mut self,
        position: DocumentPosition,
        cx: &mut Context<Self>,
    ) {
        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        self.jump_forward.clear();
        self.position_from_search = false;
        self.set_zoom(position.zoom, cx);
        self.set_scroll_doc(position.scroll_x_doc, position.scroll_y_doc, cx);
    }

    pub(crate) fn open_library_document(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        cx: &mut Context<Self>,
    ) {
        self.flush_current_history_position(cx);

        let path = normalize_path(&path);
        let initial_position =
            position.or_else(|| cx.global::<LibraryStore>().history_position(&path));
        let focus_handle = self.focus_handle.clone();
        let transparency_enabled = self.transparency_enabled;
        let (behavior, window_settings, theme) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.behavior,
                store.settings.window,
                store.resolved_theme,
            )
        };
        let annotations = PdfAnnotations {
            highlights: cx.global::<LibraryStore>().highlights_for(&path),
            notes: cx.global::<LibraryStore>().notes_for(&path),
        };

        match Self::open(
            &path,
            focus_handle,
            behavior,
            window_settings,
            theme,
            initial_position,
            annotations,
        ) {
            Ok(mut pdf_view) => {
                pdf_view.transparency_enabled = transparency_enabled;
                *self = pdf_view;
                self.flush_current_history_position(cx);
                cx.notify();
            }
            Err(error) => {
                eprintln!("failed to open {}: {error:#}", path.display());
            }
        }
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
        self.panning = None;
        self.context_menu_image =
            if self.context_menu_highlight.is_some() || self.context_menu_note.is_some() {
                None
            } else {
                self.image_at_screen(position)
            };
        self.context_menu_position = Some(position);
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    fn close_context_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let was_open = self.context_menu_position.take().is_some();
        self.context_menu_image = None;
        self.context_menu_highlight = None;
        if was_open {
            self.context_menu_note = None;
            cx.notify();
        }
        was_open
    }

    fn context_menu_entry(
        label: &'static str,
        theme: Theme,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .h(px(30.0))
            .px_3()
            .text_size(px(13.0))
            .text_color(rgb(theme.text))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(theme.control_hover)))
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

    fn context_menu_label(label: &'static str, theme: Theme) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .h(px(24.0))
            .px_3()
            .text_size(px(12.0))
            .text_color(rgb(theme.text_muted))
            .child(label)
    }

    fn render_note_popup(body: SharedString, theme: Theme) -> impl IntoElement {
        let mut note = div()
            .occlude()
            .flex()
            .flex_col()
            .gap_1()
            .max_w(px(360.0))
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(rgb(theme.panel_border))
            .shadow_lg()
            .bg(rgb(theme.panel_background))
            .text_size(px(13.0))
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

    fn render_top_aligned_overlay(
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
            .items_start()
            .justify_center()
            .pt(px(96.0))
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

    fn render_sidebar_shell(
        title: &'static str,
        count: usize,
        width: f32,
        content: impl IntoElement + 'static,
        theme: Theme,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .flex_col()
            .min_h_0()
            .w(px(width))
            .border_r(px(1.0))
            .border_color(rgb(theme.panel_border))
            .bg(rgb(theme.panel_background))
            .text_size(px(12.0))
            .text_color(rgb(theme.text))
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
                    .h(px(40.0))
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.panel_border))
                    .child(div().text_size(px(13.0)).child(title))
                    .child(
                        div()
                            .text_color(rgb(theme.text_muted))
                            .child(format!("{} items", count)),
                    ),
            )
            .child(content)
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
            .w(px(22.0))
            .h(px(22.0))
            .rounded_sm()
            .border_1()
            .border_color(if current == Some(color) {
                rgb(theme.accent)
            } else {
                rgb(theme.panel_border)
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
        div().h(px(1.0)).my_1().mx_1().bg(rgb(theme.panel_border))
    }

    fn render_context_menu(
        has_image: bool,
        has_selection: bool,
        highlight_color: Option<HighlightColor>,
        has_note: bool,
        is_fullscreen: bool,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let fullscreen_label = if is_fullscreen {
            "Exit Full Screen"
        } else {
            "Enter Full Screen"
        };

        div()
            .occlude()
            .flex()
            .flex_col()
            .w(px(190.0))
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(theme.panel_border))
            .shadow_lg()
            .overflow_hidden()
            .bg(rgb(theme.panel_background))
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
            .when(has_note, |menu| {
                menu.child(Self::context_menu_entry(
                    "Edit Note",
                    theme,
                    |this, window, cx| this.begin_edit_context_note(window, cx),
                    cx,
                ))
                .child(Self::context_menu_entry(
                    "Remove Note",
                    theme,
                    |this, _, cx| this.remove_context_note(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(!has_note && has_selection, |menu| {
                menu.child(Self::context_menu_entry(
                    "Add Note",
                    theme,
                    |this, window, cx| this.begin_add_note(window, cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when_some(highlight_color, |menu, color| {
                menu.child(Self::context_menu_label("Change Highlight Color", theme))
                    .child(Self::context_menu_highlight_color_row(
                        HighlightColorAction::Change,
                        Some(color),
                        theme,
                        cx,
                    ))
                    .child(Self::context_menu_entry(
                        "Remove Highlight",
                        theme,
                        |this, _, cx| this.remove_context_highlight(cx),
                        cx,
                    ))
                    .child(Self::context_menu_separator(theme))
            })
            .when(highlight_color.is_none() && has_selection, |menu| {
                menu.child(Self::context_menu_label("Highlight", theme))
                    .child(Self::context_menu_highlight_color_row(
                        HighlightColorAction::Create,
                        None,
                        theme,
                        cx,
                    ))
                    .child(Self::context_menu_separator(theme))
            })
            .when(has_image, |menu| {
                menu.child(Self::context_menu_entry(
                    "Copy Image",
                    theme,
                    |this, _, cx| this.copy_image(cx),
                    cx,
                ))
                .child(Self::context_menu_entry(
                    "Save Image",
                    theme,
                    |this, _, cx| this.save_image(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(highlight_color.is_none() && has_selection, |menu| {
                menu.child(Self::context_menu_entry(
                    "Copy",
                    theme,
                    |this, _, cx| this.copy_selection(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .child(Self::context_menu_entry(
                "Select All",
                theme,
                |this, _, cx| {
                    this.select_all();
                    cx.notify();
                },
                cx,
            ))
            .child(Self::context_menu_separator(theme))
            .child(Self::context_menu_entry(
                "Zoom In",
                theme,
                |this, _, cx| this.zoom_in(cx),
                cx,
            ))
            .child(Self::context_menu_entry(
                "Zoom Out",
                theme,
                |this, _, cx| this.zoom_out(cx),
                cx,
            ))
            .child(Self::context_menu_entry(
                "Reset Zoom",
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
                "Settings",
                theme,
                |this, window, cx| this.toggle_settings_panel(window, cx),
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

    fn render_toc_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.toc_sidebar_open;

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(px(20.0))
            .px_2()
            .mr_2()
            .rounded_sm()
            .text_color(rgb(if open {
                theme.accent
            } else {
                theme.footer_text
            }))
            .bg(if open {
                rgba(theme.selected_row)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.control_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.toggle_toc_sidebar(cx);
                    cx.stop_propagation();
                }),
            )
            .child("☰")
    }

    fn render_notes_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.notes_sidebar_open && self.has_navigable_notes();

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(px(20.0))
            .px_2()
            .mr_2()
            .rounded_sm()
            .text_color(rgb(if open {
                theme.accent
            } else {
                theme.footer_text
            }))
            .bg(if open {
                rgba(theme.selected_row)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.control_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.toggle_notes_sidebar(cx);
                    cx.stop_propagation();
                }),
            )
            .child("✎")
    }

    fn render_toc_sidebar(
        &mut self,
        theme: Theme,
        _window: &mut Window,
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
            .child(
                uniform_list(
                    "toc-sidebar-list",
                    visible_len,
                    cx.processor(|this, visible_range: Range<usize>, _window, cx| {
                        let theme = cx.global::<SettingsStore>().resolved_theme;
                        let current_page = this.current_page_index();

                        visible_range
                            .filter_map(|visible_ix| {
                                let entry_ix = this
                                    .toc_sidebar_visible_entry_ixs
                                    .get(visible_ix)
                                    .copied()?;
                                let entry = &this.toc[entry_ix];
                                let collapsed = this.toc_collapsed.contains(&entry_ix);
                                let current = entry.navigable && entry.page_idx == current_page;
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
                                        .flex()
                                        .flex_row()
                                        .w_full()
                                        .items_center()
                                        .h(px(TOC_SIDEBAR_ROW_HEIGHT))
                                        .px_2()
                                        .bg(if current {
                                            rgba(theme.selected_row)
                                        } else {
                                            rgba(0x00000000)
                                        })
                                        .child(div().flex_none().w(px(depth as f32 * 14.0)))
                                        .child(
                                            div()
                                                .flex()
                                                .flex_none()
                                                .items_center()
                                                .justify_center()
                                                .w(px(18.0))
                                                .h(px(24.0))
                                                .rounded_sm()
                                                .text_color(rgb(theme.text_muted))
                                                .when(has_children, |this| {
                                                    this.cursor_pointer()
                                                        .hover(move |style| {
                                                            style.bg(rgb(theme.control_hover))
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
                                                .flex()
                                                .flex_1()
                                                .min_w_0()
                                                .items_center()
                                                .h(px(28.0))
                                                .rounded_sm()
                                                .px_1()
                                                .truncate()
                                                .text_color(rgb(title_color))
                                                .when(navigable, |this| {
                                                    this.cursor_pointer()
                                                        .hover(move |style| {
                                                            style.bg(rgb(theme.control_hover))
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
                .size_full(),
            );

        Self::render_sidebar_shell(
            "Table of Contents",
            self.toc.len(),
            TOC_SIDEBAR_WIDTH,
            content,
            theme,
        )
    }

    fn render_notes_sidebar(
        &mut self,
        theme: Theme,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let visible_len = self.notes_sidebar_entries.len();
        let scroll_handle = self.notes_sidebar_scroll.clone();
        let content = div()
            .id("notes-sidebar-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(
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
                                        .flex()
                                        .flex_row()
                                        .w_full()
                                        .items_center()
                                        .h(px(NOTES_SIDEBAR_ROW_HEIGHT))
                                        .px_2()
                                        .gap_2()
                                        .bg(if current {
                                            rgba(theme.selected_row)
                                        } else {
                                            rgba(0x00000000)
                                        })
                                        .cursor_pointer()
                                        .hover(move |style| style.bg(rgb(theme.control_hover)))
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
                                                .rounded_sm()
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
                                                        .text_size(px(13.0))
                                                        .text_color(rgb(theme.text))
                                                        .child(body),
                                                )
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_size(px(11.0))
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
                .size_full(),
            );

        Self::render_sidebar_shell("Notes", visible_len, NOTES_SIDEBAR_WIDTH, content, theme)
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
            let rect = Self::quad_rect(&glyph.quad);

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
            let rect = Self::quad_rect(&glyph.quad);

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
        let scale_x = image_bounds.size.width.as_f32() / page.width;
        let scale_y = image_bounds.size.height.as_f32() / page.height;
        let x0 = image_bounds.origin.x.as_f32() + (rect.x0 - page.bounds.x0) * scale_x;
        let y0 = image_bounds.origin.y.as_f32() + (rect.y0 - page.bounds.y0) * scale_y;
        let x1 = image_bounds.origin.x.as_f32() + (rect.x1 - page.bounds.x0) * scale_x;
        let y1 = image_bounds.origin.y.as_f32() + (rect.y1 - page.bounds.y0) * scale_y;

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

        dx.hypot(dy)
    }

    fn with_alpha(rgb: u32, alpha: u8) -> u32 {
        ((rgb & 0x00ff_ffff) << 8) | alpha as u32
    }
}

impl Render for PdfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let prepaint_view = view.clone();
        let (theme, window_settings) = {
            let store = cx.global::<SettingsStore>();
            (store.resolved_theme, store.settings.window)
        };
        let opacity = window_settings.effective_opacity(self.transparency_enabled);
        self.step_scroll_animation(window, cx);
        let context_menu_highlight_color = self
            .context_menu_highlight
            .and_then(|id| self.highlight_color_for_id(id));
        let context_menu = self.context_menu_position.map(|position| {
            deferred(
                anchored()
                    .snap_to_window_with_margin(px(8.0))
                    .position(position)
                    .anchor(Anchor::TopLeft)
                    .child(Self::render_context_menu(
                        self.context_menu_image.is_some(),
                        self.selected_text().is_some(),
                        context_menu_highlight_color,
                        self.context_menu_note.is_some(),
                        window.is_fullscreen(),
                        theme,
                        cx,
                    )),
            )
            .with_priority(1)
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

        div()
            .size_full()
            .flex()
            .flex_col()
            .cursor_text()
            .when(
                self.hovered_link.is_some() || self.hovered_note.is_some(),
                |this| this.cursor_pointer(),
            )
            .when(self.panning.is_some(), |this| this.cursor_grabbing())
            .key_context("PdfView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| {
                this.copy_selection(cx);
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
            .on_action(cx.listener(|this, _: &Search, window, cx| {
                this.toggle_search(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SearchNext, _, cx| {
                this.navigate_search(false, cx);
            }))
            .on_action(cx.listener(|this, _: &SearchPrev, _, cx| {
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
            .on_action(cx.listener(|this, _: &JumpBackward, _, cx| {
                this.jump_backward(cx);
            }))
            .on_action(cx.listener(|this, _: &JumpForward, _, cx| {
                this.jump_forward(cx);
            }))
            .on_action(cx.listener(|this, _: &Toc, window, cx| {
                this.toggle_toc_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleToc, _, cx| {
                this.toggle_toc_sidebar(cx);
            }))
            .on_action(cx.listener(|this, _: &OpenNotes, window, cx| {
                this.toggle_notes_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleNotes, _, cx| {
                this.toggle_notes_sidebar(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(cx.listener(|_, _: &ToggleFullScreen, window, _| {
                window.toggle_fullscreen();
            }))
            .on_action(cx.listener(|this, _: &ToggleTransparency, window, cx| {
                this.toggle_transparency(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.toggle_settings_panel(window, cx);
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
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, _| {
                this.control_down = event.modifiers.control;
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                this.handle_scroll_wheel(event, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.close_context_menu(cx);
                    this.focus_handle.focus(window, cx);
                    this.dragging = false;
                    this.pending_link = None;

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
                            this.selection = Some(Selection {
                                anchor: ix,
                                head: ix,
                            });
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
                    this.focus_handle.focus(window, cx);
                    this.dragging = false;
                    this.pending_link = None;
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
                    if this.command_palette.is_some()
                        || this.settings_panel.is_some()
                        || this.search_bar.is_some()
                        || this.toc_palette.is_some()
                        || this.history_palette.is_some()
                        || this.bookmark_palette.is_some()
                        || this.bookmark_prompt.is_some()
                        || this.note_editor.is_some()
                    {
                        window.prevent_default();
                        cx.stop_propagation();
                        return;
                    }

                    this.context_menu_highlight = this.highlight_at_screen(event.position);
                    this.context_menu_note = this.note_at_screen(event.position);
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
                    return;
                }

                this.set_hovered_link(None, cx);
                this.set_hovered_note(None, None, cx);
                if let Some(ix) = this.glyph_at_screen(event.position) {
                    if let Some(selection) = this.selection.as_mut() {
                        selection.head = ix;
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    let pending_link = this.pending_link.take();
                    this.dragging = false;
                    if let Some(link_ix) = pending_link {
                        if this.link_at_screen(event.position) == Some(link_ix) {
                            this.activate_link(link_ix, cx);
                            cx.stop_propagation();
                        }
                        cx.notify();
                    }
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
                    .bg(rgb(theme.background))
                    .when(opacity < 1.0, |this| this.opacity(opacity))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_row()
                            .when(self.toc_sidebar_open && !self.toc.is_empty(), |this| {
                                this.child(self.render_toc_sidebar(theme, window, cx))
                            })
                            .when(
                                self.notes_sidebar_open && self.has_navigable_notes(),
                                |this| this.child(self.render_notes_sidebar(theme, window, cx)),
                            )
                            .child(
                                div().flex_1().min_w_0().min_h_0().overflow_hidden().child(
                                    canvas(
                                        move |bounds, window, cx| {
                                            prepaint_view.update(cx, |this, cx| {
                                                this.paint_data(bounds, window.scale_factor(), cx)
                                                    .expect("failed to render PDF")
                                            })
                                        },
                                        move |_, paint_data, window, _| {
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
                                        },
                                    )
                                    .size_full(),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .w_full()
                            .flex_none()
                            .h(px(26.0))
                            .px_3()
                            .border_t(px(1.0))
                            .border_color(rgb(theme.footer_border))
                            .bg(rgb(theme.footer_background))
                            .text_size(px(12.0))
                            .text_color(rgb(theme.footer_text))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .child(self.render_toc_button(theme, cx))
                                    .child(self.render_notes_button(theme, cx))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(self.file_name.clone()),
                                    ),
                            )
                            .child(div().flex_none().child(format!(
                                "{} / {}",
                                self.current_page(),
                                self.pages.len()
                            ))),
                    ),
            )
            .when_some(self.command_palette.clone(), |this, palette| {
                this.child(Self::render_top_aligned_overlay(
                    palette,
                    |this, window, cx| this.close_command_palette(window, cx),
                    theme,
                    cx,
                ))
            })
            .when_some(self.settings_panel.clone(), |this, panel| {
                this.child(Self::render_top_aligned_overlay(
                    panel,
                    |this, window, cx| this.close_settings_panel(window, cx),
                    theme,
                    cx,
                ))
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
            .when_some(self.toc_palette.clone(), |this, palette| {
                this.child(Self::render_centered_overlay(
                    palette,
                    |this, window, cx| this.close_toc_palette(window, cx),
                    theme,
                    cx,
                ))
            })
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
    }
}

#[cfg(test)]
mod tests {
    use super::{Glyph, PdfView};
    use mupdf::{Point, Quad};

    fn test_glyph(c: char, x0: f32, x1: f32) -> Glyph {
        Glyph {
            c,
            quad: Quad::new(
                Point::new(x0, 0.0),
                Point::new(x1, 0.0),
                Point::new(x0, 6.0),
                Point::new(x1, 6.0),
            ),
            page_idx: 0,
            line_idx: 0,
            block_idx: 0,
        }
    }

    fn missing_space_glyphs() -> Vec<Glyph> {
        [
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
        .collect()
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
}
