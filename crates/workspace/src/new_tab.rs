use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Local, Utc};
use glyphst_core::DocumentFormat;
use gpui::{
    AnyElement, Context, Corners, EventEmitter, FocusHandle, IntoElement, Modifiers, MouseButton,
    MouseDownEvent, Pixels, Render, RenderImage, ScrollStrategy, SharedString,
    UniformListScrollHandle, Window, canvas, div, fill, point, prelude::*, px, rgb,
    transparent_black, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _, ThemeColor,
    alert::Alert,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
};
use image::{Frame, ImageFormat, RgbaImage};
use mupdf::{Colorspace, Device, DisplayList, Matrix, Pixmap, Rect};
use smallvec::smallvec;

pub use glyphst_actions::new_tab::{
    ActivateFocused, CancelSelect, CursorDown, CursorFirst, CursorLast, CursorLeft, CursorRight,
    CursorUp, DeleteSelected, EnterSelect, ExtendSelectionDown, ExtendSelectionFirst,
    ExtendSelectionLast, ExtendSelectionLeft, ExtendSelectionRight, ExtendSelectionUp, SelectAll,
    ToggleFocused,
};

use crate::{
    document_view::{NextHalfPage, PreviousHalfPage, ScrollLineDown, ScrollLineUp},
    icons::{Icon, IconName},
    library::{HistoryEntry, LibraryStore},
    scrollbar::{ScrollbarHandle as _, WithScrollbar as _},
    settings::{NewTabViewMode, SettingsStore, ui_rems},
};

const BOARD_WIDTH: f32 = 900.0;
const BOARD_HEIGHT: f32 = 700.0;
const CARD_WIDTH: f32 = 220.0;
const CARD_HEIGHT: f32 = 330.0;
const THUMBNAIL_MAX_WIDTH: f32 = 170.0;
const THUMBNAIL_MAX_HEIGHT: f32 = 240.0;
const GRID_GAP: f32 = 18.0;
const GRID_PADDING_X: f32 = 24.0;
const VIEWPORT_WIDTH_EPSILON: f32 = 0.5;
const DEFAULT_COLS: usize = 3;
const GRID_ROW_HEIGHT: f32 = CARD_HEIGHT + GRID_GAP;
const LIST_ROW_HEIGHT: f32 = 38.0;
const DETAILS_HEADER_HEIGHT: f32 = 32.0;
const DETAILS_LOCATION_BREAKPOINT: f32 = 760.0;
const DETAILS_FORMAT_SIZE_BREAKPOINT: f32 = 620.0;
const DETAILS_LAST_OPENED_BREAKPOINT: f32 = 480.0;
const NEW_TAB_HEADER_HEIGHT: f32 = 40.0;
const THUMBNAIL_CACHE_VERSION: &str = "v1";
const MAX_PERSISTENT_THUMBNAILS: usize = 256;

#[derive(Clone)]
struct ThumbnailImage {
    image: Arc<RenderImage>,
    width: f32,
    height: f32,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ThumbnailCacheKey {
    path: PathBuf,
    size: u64,
    modified_nanos: u128,
}

impl ThumbnailCacheKey {
    fn for_path(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        let modified_nanos = metadata
            .modified()
            .unwrap_or(UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Ok(Self {
            path: path.to_path_buf(),
            size: metadata.len(),
            modified_nanos,
        })
    }

    fn disk_path(&self) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        thumbnail_cache_dir().join(format!("{:016x}.png", hasher.finish()))
    }
}

pub struct ThumbnailCache {
    images: HashMap<ThumbnailCacheKey, ThumbnailImage>,
    order: VecDeque<ThumbnailCacheKey>,
}

impl gpui::Global for ThumbnailCache {}

impl ThumbnailCache {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &ThumbnailCacheKey) -> Option<ThumbnailImage> {
        let image = self.images.get(key).cloned()?;
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(key.clone());
        Some(image)
    }

    fn insert(&mut self, key: ThumbnailCacheKey, image: ThumbnailImage) {
        self.images
            .retain(|existing, _| existing.path != key.path || existing == &key);
        self.order
            .retain(|existing| existing.path != key.path || existing == &key);
        self.images.insert(key.clone(), image);
        self.order.retain(|existing| existing != &key);
        self.order.push_back(key);
        while self.images.len() > MAX_PERSISTENT_THUMBNAILS {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.images.remove(&oldest);
        }
    }
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
enum ThumbnailState {
    Loading,
    Ready(ThumbnailImage),
    Failed,
}

struct NewTabItemState {
    item_ix: usize,
    focused: bool,
    selecting: bool,
    selected: bool,
}

struct NewTabCardState {
    thumbnail: ThumbnailState,
    item: NewTabItemState,
}

#[derive(Clone)]
struct NewTabEntry {
    path: PathBuf,
    file_name: SharedString,
    directory: SharedString,
    last_read: SharedString,
    progress: SharedString,
    format_size: SharedString,
    last_opened: SharedString,
    format: Option<DocumentFormat>,
}

impl NewTabEntry {
    fn icon(&self) -> IconName {
        match self.format {
            Some(DocumentFormat::Pdf) => IconName::FileText,
            Some(DocumentFormat::Epub) => IconName::BookOpen,
            None => IconName::File,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NewTabItemKey {
    OpenFile,
    Document(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CursorMove {
    Up,
    Down,
    Left,
    Right,
    First,
    Last,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DetailsColumns {
    location: bool,
    format_size: bool,
    last_opened: bool,
}

impl DetailsColumns {
    fn for_width(width: f32) -> Self {
        if !width.is_finite() || width <= 0.0 {
            return Self {
                location: true,
                format_size: true,
                last_opened: true,
            };
        }

        Self {
            location: width >= DETAILS_LOCATION_BREAKPOINT,
            format_size: width >= DETAILS_FORMAT_SIZE_BREAKPOINT,
            last_opened: width >= DETAILS_LAST_OPENED_BREAKPOINT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NewTabEvent {
    OpenFile,
    OpenDocument(PathBuf),
}

pub struct NewTab {
    entries: Vec<NewTabEntry>,
    thumbnails: HashMap<PathBuf, ThumbnailState>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    cols: usize,
    viewport_width: f32,
    selecting: bool,
    selection: BTreeSet<PathBuf>,
    focused_item: NewTabItemKey,
    selection_anchor: Option<PathBuf>,
    observed_view_mode: NewTabViewMode,
}

impl NewTab {
    pub const WINDOW_WIDTH: f32 = BOARD_WIDTH;
    pub const WINDOW_HEIGHT: f32 = BOARD_HEIGHT;

    pub(crate) fn new(focus_handle: FocusHandle, cx: &mut Context<Self>) -> Self {
        if !cx.has_global::<ThumbnailCache>() {
            cx.set_global(ThumbnailCache::new());
        }
        let entries = {
            let store = cx.global_mut::<LibraryStore>();
            if let Err(error) = store.remove_missing_history_entries() {
                eprintln!("failed to save pruned PDF history: {error:#}");
            }

            store
                .history_entries()
                .iter()
                .map(NewTabEntry::from_history)
                .collect::<Vec<_>>()
        };

        let observed_view_mode = cx.global::<SettingsStore>().settings.new_tab.view_mode;
        let board = Self {
            entries,
            thumbnails: HashMap::new(),
            focus_handle,
            scroll: UniformListScrollHandle::new(),
            cols: DEFAULT_COLS,
            viewport_width: 0.0,
            selecting: false,
            selection: BTreeSet::new(),
            focused_item: NewTabItemKey::OpenFile,
            selection_anchor: None,
            observed_view_mode,
        };

        cx.observe_global::<SettingsStore>(|this, cx| {
            let mode = cx.global::<SettingsStore>().settings.new_tab.view_mode;
            if this.observed_view_mode != mode {
                this.observed_view_mode = mode;
                this.scroll_focused(mode);
            }
            cx.notify();
        })
        .detach();
        board
    }

    fn start_thumbnail_render(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.thumbnails.contains_key(&path) {
            return;
        }
        let key = match ThumbnailCacheKey::for_path(&path) {
            Ok(key) => key,
            Err(error) => {
                eprintln!(
                    "failed to fingerprint new tab preview for {}: {error:#}",
                    path.display()
                );
                self.thumbnails.insert(path, ThumbnailState::Failed);
                return;
            }
        };
        if let Some(image) = cx.global_mut::<ThumbnailCache>().get(&key) {
            self.thumbnails.insert(path, ThumbnailState::Ready(image));
            return;
        }

        self.thumbnails
            .insert(path.clone(), ThumbnailState::Loading);

        cx.spawn(async move |this, cx| {
            let render_path = path.clone();
            let render_key = key.clone();
            let result = cx
                .background_spawn(async move {
                    load_or_render_cover_thumbnail(&render_path, &render_key)
                })
                .await;

            let state = match result {
                Ok(image) => ThumbnailState::Ready(image),
                Err(error) => {
                    eprintln!(
                        "failed to render new tab preview for {}: {error:#}",
                        path.display()
                    );
                    ThumbnailState::Failed
                }
            };

            let _ = this.update(cx, |this, cx| {
                if let ThumbnailState::Ready(image) = &state {
                    cx.global_mut::<ThumbnailCache>().insert(key, image.clone());
                }
                this.thumbnails.insert(path, state);
                cx.notify();
            });
        })
        .detach();
    }

    fn cols_for_width(viewport_width: f32) -> usize {
        if !viewport_width.is_finite() || viewport_width <= 0.0 {
            return DEFAULT_COLS;
        }

        let available_width = (viewport_width - GRID_PADDING_X * 2.0).max(1.0);
        (((available_width + GRID_GAP) / (CARD_WIDTH + GRID_GAP)).floor() as usize).max(1)
    }

    fn horizontal_gap(viewport_width: f32, cols: usize) -> f32 {
        if cols <= 1 || !viewport_width.is_finite() || viewport_width <= 0.0 {
            return GRID_GAP;
        }

        let available_width = (viewport_width - GRID_PADDING_X * 2.0).max(1.0);
        let cards_width = CARD_WIDTH * cols as f32;
        ((available_width - cards_width) / (cols - 1) as f32).max(GRID_GAP)
    }

    fn update_viewport(&mut self, viewport_width: f32, cx: &mut Context<Self>) {
        if !viewport_width.is_finite() || viewport_width <= 0.0 {
            return;
        }

        let width_changed = (viewport_width - self.viewport_width).abs() > VIEWPORT_WIDTH_EPSILON;
        self.viewport_width = viewport_width;
        let cols = Self::cols_for_width(viewport_width);
        if cols == self.cols && !width_changed {
            return;
        }

        self.cols = cols;
        cx.notify();
    }

    fn view_mode(&self, cx: &Context<Self>) -> NewTabViewMode {
        cx.global::<SettingsStore>().settings.new_tab.view_mode
    }

    fn set_view_mode(&mut self, mode: NewTabViewMode, cx: &mut Context<Self>) {
        if self.view_mode(cx) == mode {
            return;
        }

        let mut settings = cx.global::<SettingsStore>().settings.clone();
        settings.new_tab.view_mode = mode;
        if let Err(error) = cx
            .global_mut::<SettingsStore>()
            .set_settings(settings.clone())
        {
            eprintln!("failed to persist new tab view mode: {error:#}");
            cx.global_mut::<SettingsStore>()
                .set_settings_in_memory(settings);
        }

        self.scroll_focused(mode);
        cx.refresh_windows();
    }

    fn item_count(&self) -> usize {
        self.entries.len() + 1
    }

    fn item_key(&self, item_ix: usize) -> Option<NewTabItemKey> {
        if item_ix == 0 {
            Some(NewTabItemKey::OpenFile)
        } else {
            self.entries
                .get(item_ix - 1)
                .map(|entry| NewTabItemKey::Document(entry.path.clone()))
        }
    }

    fn item_index(&self, key: &NewTabItemKey) -> Option<usize> {
        match key {
            NewTabItemKey::OpenFile => Some(0),
            NewTabItemKey::Document(path) => self
                .entries
                .iter()
                .position(|entry| &entry.path == path)
                .map(|entry_ix| entry_ix + 1),
        }
    }

    fn entry_for_item(&self, item_ix: usize) -> Option<&NewTabEntry> {
        item_ix
            .checked_sub(1)
            .and_then(|entry_ix| self.entries.get(entry_ix))
    }

    fn focused_index(&self) -> usize {
        self.item_index(&self.focused_item).unwrap_or(0)
    }

    fn set_focused_index(&mut self, item_ix: usize) {
        if let Some(key) = self.item_key(item_ix.min(self.item_count().saturating_sub(1))) {
            self.focused_item = key;
        }
    }

    fn row_for_item(&self, item_ix: usize, mode: NewTabViewMode) -> usize {
        match mode {
            NewTabViewMode::Grid => item_ix / self.cols.max(1),
            NewTabViewMode::List | NewTabViewMode::Details => item_ix,
        }
    }

    fn scroll_focused(&self, mode: NewTabViewMode) {
        self.scroll.scroll_to_item(
            self.row_for_item(self.focused_index(), mode),
            ScrollStrategy::Center,
        );
    }

    fn row_height(mode: NewTabViewMode) -> f32 {
        match mode {
            NewTabViewMode::Grid => GRID_ROW_HEIGHT,
            NewTabViewMode::List | NewTabViewMode::Details => LIST_ROW_HEIGHT,
        }
    }

    fn scroll_by(&mut self, delta_y: Pixels, cx: &mut Context<Self>) {
        let max_offset = self.scroll.0.borrow().base_handle.max_offset().y;
        if max_offset <= px(0.) {
            return;
        }

        let current = self.scroll.offset();
        let new_y = (current.y - delta_y).clamp(-max_offset, px(0.));
        self.scroll.set_offset(point(current.x, new_y));
        cx.notify();
    }

    fn enter_select(&mut self, cx: &mut Context<Self>) {
        self.selecting = true;
        if self.focused_item == NewTabItemKey::OpenFile
            && let Some(first) = self.entries.first()
        {
            self.focused_item = NewTabItemKey::Document(first.path.clone());
        }
        cx.notify();
    }

    fn cancel_select(&mut self, cx: &mut Context<Self>) {
        let changed = self.selecting || !self.selection.is_empty();
        self.selecting = false;
        self.selection.clear();
        self.selection_anchor = None;
        if changed {
            cx.notify();
        }
    }

    fn toggle_selection(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.selecting = true;
        self.focused_item = NewTabItemKey::Document(path.clone());
        self.selection_anchor = Some(path.clone());
        if !self.selection.insert(path.clone()) {
            self.selection.remove(&path);
        }
        cx.notify();
    }

    fn selection_range(
        entries: &[NewTabEntry],
        anchor: &Path,
        target_item_ix: usize,
    ) -> BTreeSet<PathBuf> {
        let Some(anchor_entry_ix) = entries.iter().position(|entry| entry.path == anchor) else {
            return BTreeSet::new();
        };
        let target_entry_ix = target_item_ix
            .saturating_sub(1)
            .min(entries.len().saturating_sub(1));
        let (start, end) = if anchor_entry_ix <= target_entry_ix {
            (anchor_entry_ix, target_entry_ix)
        } else {
            (target_entry_ix, anchor_entry_ix)
        };
        entries[start..=end]
            .iter()
            .map(|entry| entry.path.clone())
            .collect()
    }

    fn extend_selection_to(&mut self, target_item_ix: usize, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            return;
        }

        let anchor = self.selection_anchor.clone().or_else(|| {
            self.entry_for_item(self.focused_index())
                .map(|entry| entry.path.clone())
        });
        let target_item_ix = if target_item_ix == 0 {
            1
        } else {
            target_item_ix.min(self.item_count() - 1)
        };
        let Some(anchor) = anchor.or_else(|| {
            self.entry_for_item(target_item_ix)
                .map(|entry| entry.path.clone())
        }) else {
            return;
        };

        self.selecting = true;
        self.selection_anchor = Some(anchor.clone());
        self.selection = Self::selection_range(&self.entries, &anchor, target_item_ix);
        self.set_focused_index(target_item_ix);
        cx.notify();
    }

    fn handle_document_pointer(
        &mut self,
        path: PathBuf,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let target_item_ix = self
            .entries
            .iter()
            .position(|entry| entry.path == path)
            .map(|entry_ix| entry_ix + 1)
            .unwrap_or(0);

        if modifiers.shift {
            self.extend_selection_to(target_item_ix, cx);
        } else if modifiers.secondary() || self.selecting {
            self.toggle_selection(path, cx);
        } else {
            self.focused_item = NewTabItemKey::Document(path.clone());
            self.selection_anchor = Some(path.clone());
            self.open_document(path, cx);
        }
    }

    fn target_index(&self, movement: CursorMove, mode: NewTabViewMode) -> usize {
        let current = self.focused_index();
        let last = self.item_count().saturating_sub(1);
        match movement {
            CursorMove::First => 0,
            CursorMove::Last => last,
            CursorMove::Left | CursorMove::Up if mode != NewTabViewMode::Grid => {
                current.saturating_sub(1)
            }
            CursorMove::Right | CursorMove::Down if mode != NewTabViewMode::Grid => {
                current.saturating_add(1).min(last)
            }
            CursorMove::Left => current.saturating_sub(1),
            CursorMove::Right => current.saturating_add(1).min(last),
            CursorMove::Up => current.saturating_sub(self.cols.max(1)),
            CursorMove::Down => current.saturating_add(self.cols.max(1)).min(last),
        }
    }

    fn move_cursor(&mut self, movement: CursorMove, extend: bool, cx: &mut Context<Self>) {
        let mode = self.view_mode(cx);
        let target = self.target_index(movement, mode);
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = self
                    .entry_for_item(self.focused_index())
                    .map(|entry| entry.path.clone())
                    .or_else(|| self.entry_for_item(target).map(|entry| entry.path.clone()));
            }
            self.extend_selection_to(target, cx);
        } else {
            self.set_focused_index(target);
            cx.notify();
        }
        self.scroll_focused(mode);
    }

    fn activate_focused(&mut self, cx: &mut Context<Self>) {
        match self.focused_item.clone() {
            NewTabItemKey::OpenFile => self.open_file(cx),
            NewTabItemKey::Document(path) => self.open_document(path, cx),
        }
    }

    fn toggle_focused(&mut self, cx: &mut Context<Self>) {
        let path = match self.focused_item.clone() {
            NewTabItemKey::Document(path) => path,
            NewTabItemKey::OpenFile => {
                let Some(first) = self.entries.first() else {
                    self.enter_select(cx);
                    return;
                };
                first.path.clone()
            }
        };
        self.toggle_selection(path, cx);
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            return;
        }
        self.selecting = true;
        self.selection = self
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        if self.selection_anchor.is_none() {
            self.selection_anchor = self.entries.first().map(|entry| entry.path.clone());
        }
        if self.focused_item == NewTabItemKey::OpenFile {
            self.focused_item = NewTabItemKey::Document(self.entries[0].path.clone());
        }
        cx.notify();
    }

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            return;
        }

        let paths = self.selection.iter().cloned().collect::<Vec<_>>();
        let focused_ix = self.focused_index();

        {
            let store = cx.global_mut::<LibraryStore>();
            if let Err(error) = store.remove_history_entries(&paths) {
                eprintln!("failed to remove history entries: {error:#}");
            }
        }

        for path in &paths {
            self.thumbnails.remove(path);
        }

        self.refresh_entries(cx);
        if self.item_index(&self.focused_item).is_none() {
            self.set_focused_index(focused_ix.min(self.item_count().saturating_sub(1)));
        }
        self.selecting = false;
        self.selection.clear();
        self.selection_anchor = None;
        cx.notify();
    }

    fn refresh_entries(&mut self, cx: &mut Context<Self>) {
        let store = cx.global::<LibraryStore>();
        self.entries = store
            .history_entries()
            .iter()
            .map(NewTabEntry::from_history)
            .collect();
        let existing = self
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<BTreeSet<_>>();
        self.selection.retain(|path| existing.contains(path));
        if self
            .selection_anchor
            .as_ref()
            .is_some_and(|path| !existing.contains(path))
        {
            self.selection_anchor = None;
        }
    }

    fn render_toolbar_button(
        label: String,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(label.clone())
            .small()
            .outline()
            .label(label)
            .on_click(cx.listener(move |this, _, window, cx| {
                handler(this, window, cx);
                cx.stop_propagation();
            }))
    }

    fn render_view_button(
        mode: NewTabViewMode,
        current_mode: NewTabViewMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (id, icon, label) = match mode {
            NewTabViewMode::Grid => (
                "new-tab-view-grid",
                IconName::LayoutGrid,
                crate::fl!("new-tab-view-grid"),
            ),
            NewTabViewMode::List => (
                "new-tab-view-list",
                IconName::LayoutList,
                crate::fl!("new-tab-view-list"),
            ),
            NewTabViewMode::Details => (
                "new-tab-view-details",
                IconName::Table2,
                crate::fl!("new-tab-view-details"),
            ),
        };

        Button::new(id)
            .debug_selector(move || id.to_string())
            .xsmall()
            .compact()
            .ghost()
            .selected(mode == current_mode)
            .icon(icon)
            .tooltip(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_view_mode(mode, cx);
                cx.stop_propagation();
            }))
    }

    fn item_state(&self, item_ix: usize) -> NewTabItemState {
        let key = self.item_key(item_ix);
        let selected = key.as_ref().is_some_and(|key| match key {
            NewTabItemKey::OpenFile => false,
            NewTabItemKey::Document(path) => self.selection.contains(path),
        });
        NewTabItemState {
            item_ix,
            focused: key.as_ref() == Some(&self.focused_item),
            selecting: self.selecting,
            selected,
        }
    }

    fn render_open_file_card(
        state: NewTabItemState,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let border_color = if state.focused {
            theme.ring
        } else {
            theme.border
        };

        div()
            .id("new-tab-open-file-grid")
            .debug_selector(|| "new-tab-open-file-grid".to_string())
            .flex()
            .flex_none()
            .flex_col()
            .items_center()
            .relative()
            .w(px(CARD_WIDTH))
            .h(px(CARD_HEIGHT))
            .p_3()
            .border_1()
            .border_color(border_color)
            .bg(theme.popover)
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.focused_item = NewTabItemKey::OpenFile;
                    this.open_file(cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(THUMBNAIL_MAX_WIDTH))
                    .h(px(THUMBNAIL_MAX_HEIGHT))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(
                        Icon::new(IconName::FolderOpen)
                            .size(ui_rems(42.0))
                            .text_color(theme.primary),
                    ),
            )
            .child(
                div()
                    .mt_3()
                    .w_full()
                    .text_size(ui_rems(13.0))
                    .text_color(theme.primary)
                    .child(crate::fl!("new-tab-open-file")),
            )
            .into_any_element()
    }

    fn render_document_card(
        entry: &NewTabEntry,
        state: NewTabCardState,
        theme: ThemeColor,
        page_color: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let NewTabCardState { thumbnail, item } = state;
        let path = entry.path.clone();
        let file_name = entry.file_name.clone();
        let directory = entry.directory.clone();
        let last_read = entry.last_read.clone();
        let border_color = if item.focused {
            theme.ring
        } else if item.selected {
            theme.primary
        } else {
            theme.border
        };
        let card_bg = if item.selected {
            theme.primary.opacity(0x22 as f32 / 255.0)
        } else {
            theme.popover
        };

        div()
            .id(("new-tab-card", item.item_ix))
            .flex()
            .flex_none()
            .flex_col()
            .items_center()
            .relative()
            .w(px(CARD_WIDTH))
            .h(px(CARD_HEIGHT))
            .p_3()
            .border_1()
            .border_color(border_color)
            .bg(card_bg)
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.handle_document_pointer(path.clone(), event.modifiers, cx);
                    cx.stop_propagation();
                }),
            )
            .when(item.selecting, |card| {
                card.child(
                    Checkbox::new(("new-tab-card-selection", item.item_ix))
                        .xsmall()
                        .checked(item.selected)
                        .tab_stop(false)
                        .absolute()
                        .top(px(6.0))
                        .right(px(6.0))
                        .p_1()
                        .bg(theme.popover.opacity(0xe8 as f32 / 255.0))
                        .rounded(cx.theme().radius),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(THUMBNAIL_MAX_WIDTH))
                    .h(px(THUMBNAIL_MAX_HEIGHT))
                    .child(Self::render_thumbnail(thumbnail, theme, page_color)),
            )
            .child(
                div()
                    .mt_3()
                    .w_full()
                    .truncate()
                    .text_size(ui_rems(13.0))
                    .child(file_name),
            )
            .child(
                div()
                    .mt_1()
                    .w_full()
                    .truncate()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .child(directory),
            )
            .child(
                div()
                    .mt_1()
                    .w_full()
                    .truncate()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .child(last_read),
            )
    }

    fn render_open_file_row(
        state: NewTabItemState,
        details: bool,
        columns: DetailsColumns,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = if details {
            "new-tab-open-file-details"
        } else {
            "new-tab-open-file-list"
        };
        let mut row = div()
            .id(id)
            .debug_selector(move || id.to_string())
            .h(px(LIST_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .border_1()
            .border_color(if state.focused {
                theme.ring
            } else {
                transparent_black()
            })
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.focused_item = NewTabItemKey::OpenFile;
                    this.open_file(cx);
                    cx.stop_propagation();
                }),
            );

        if state.selecting {
            row = row.child(div().flex_none().w(px(18.0)));
        }
        row = row
            .child(
                Icon::new(IconName::FolderOpen)
                    .size(ui_rems(16.0))
                    .text_color(theme.primary),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.primary)
                    .child(crate::fl!("new-tab-open-file")),
            );

        if details {
            if columns.location {
                row = row.child(div().flex_1().min_w_0());
            }
            if columns.format_size {
                row = row.child(div().flex_none().w(px(112.0)));
            }
            if columns.last_opened {
                row = row.child(div().flex_none().w(px(140.0)));
            }
            row = row.child(div().flex_none().w(px(92.0)));
        }

        row.into_any_element()
    }

    fn render_document_row(
        entry: &NewTabEntry,
        state: NewTabItemState,
        details: bool,
        columns: DetailsColumns,
        show_list_location: bool,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = entry.path.clone();
        let file_name = entry.file_name.clone();
        let directory = entry.directory.clone();
        let format_size = entry.format_size.clone();
        let last_opened = entry.last_opened.clone();
        let progress = entry.progress.clone();
        let icon = entry.icon();

        let mut row = div()
            .id(if details {
                ("new-tab-details-row", state.item_ix)
            } else {
                ("new-tab-list-row", state.item_ix)
            })
            .h(px(LIST_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .border_1()
            .border_color(if state.focused {
                theme.ring
            } else if state.selected {
                theme.primary
            } else {
                transparent_black()
            })
            .bg(if state.selected {
                theme.primary.opacity(0x18 as f32 / 255.0)
            } else {
                transparent_black()
            })
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.handle_document_pointer(path.clone(), event.modifiers, cx);
                    cx.stop_propagation();
                }),
            );

        if state.selecting {
            row = row.child(
                Checkbox::new(("new-tab-row-selection", state.item_ix))
                    .xsmall()
                    .checked(state.selected)
                    .tab_stop(false),
            );
        }

        row = row
            .child(
                Icon::new(icon)
                    .size(ui_rems(16.0))
                    .text_color(theme.muted_foreground),
            )
            .child(div().flex_1().min_w_0().truncate().child(file_name));

        if details {
            if columns.location {
                row = row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(directory),
                );
            }
            if columns.format_size {
                row = row.child(
                    div()
                        .flex_none()
                        .w(px(112.0))
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(format_size),
                );
            }
            if columns.last_opened {
                row = row.child(
                    div()
                        .flex_none()
                        .w(px(140.0))
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(last_opened),
                );
            }
            row = row.child(
                div()
                    .flex_none()
                    .w(px(92.0))
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(progress),
            );
        } else if show_list_location {
            row = row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(directory),
            );
        }

        row.into_any_element()
    }

    fn render_empty_hint(height: f32, theme: ThemeColor) -> AnyElement {
        div()
            .h(px(height))
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .text_size(ui_rems(14.0))
                    .child(crate::fl!("new-tab-empty-title")),
            )
            .child(
                div()
                    .text_size(ui_rems(11.0))
                    .child(crate::fl!("new-tab-empty-description")),
            )
            .into_any_element()
    }

    fn render_thumbnail(
        thumbnail: ThumbnailState,
        theme: ThemeColor,
        page_color: u32,
    ) -> AnyElement {
        match thumbnail {
            ThumbnailState::Ready(thumbnail) => {
                let image = thumbnail.image.clone();
                canvas(
                    move |_, _, _| image.clone(),
                    move |bounds, image, window, _| {
                        window.paint_quad(fill(bounds, rgb(page_color)));
                        if let Err(error) =
                            window.paint_image(bounds, bounds, Corners::default(), image, 0, false)
                        {
                            eprintln!("failed to paint new tab preview: {error:#}");
                        }
                    },
                )
                .w(px(thumbnail.width))
                .h(px(thumbnail.height))
                .border_1()
                .border_color(theme.border)
                .into_any_element()
            }
            ThumbnailState::Loading => div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(THUMBNAIL_MAX_WIDTH))
                .h(px(THUMBNAIL_MAX_HEIGHT))
                .border_1()
                .border_color(theme.border)
                .bg(rgb(page_color))
                .text_size(ui_rems(11.0))
                .text_color(theme.muted_foreground)
                .child(crate::fl!("new-tab-rendering-preview"))
                .into_any_element(),
            ThumbnailState::Failed => div()
                .w(px(THUMBNAIL_MAX_WIDTH))
                .h(px(THUMBNAIL_MAX_HEIGHT))
                .child(
                    Alert::error(
                        "new-tab-preview-unavailable",
                        crate::fl!("new-tab-preview-unavailable"),
                    )
                    .xsmall(),
                )
                .into_any_element(),
        }
    }

    fn render_grid(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let cols = self.cols.max(1);
        let item_count = self.item_count();
        let item_rows = item_count.div_ceil(cols);
        let rows = item_rows + usize::from(self.entries.is_empty());
        let horizontal_gap = Self::horizontal_gap(self.viewport_width, cols);
        let scroll = self.scroll.clone();

        div()
            .id("new-tab-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(
                div()
                    .relative()
                    .size_full()
                    .child(
                        uniform_list(
                            "new-tab-grid",
                            rows,
                            cx.processor(move |this, visible_range: Range<usize>, _window, cx| {
                                let page_color = cx.global::<SettingsStore>().resolved_theme.page;
                                let theme = cx.theme().colors;
                                let prewarm_range = visible_range.start.saturating_sub(1)
                                    ..(visible_range.end + 1).min(item_rows);
                                let visible_paths = prewarm_range
                                    .flat_map(|row_ix| {
                                        let first_item_ix = row_ix * cols;
                                        first_item_ix..first_item_ix + cols
                                    })
                                    .filter_map(|item_ix| {
                                        this.entry_for_item(item_ix).map(|entry| entry.path.clone())
                                    })
                                    .collect::<Vec<_>>();

                                for path in visible_paths {
                                    this.start_thumbnail_render(path, cx);
                                }

                                visible_range
                                    .map(|row_ix| {
                                        if row_ix >= item_rows {
                                            return Self::render_empty_hint(GRID_ROW_HEIGHT, theme);
                                        }

                                        let mut row = div()
                                            .flex()
                                            .flex_row()
                                            .items_start()
                                            .justify_center()
                                            .gap(px(horizontal_gap))
                                            .w_full()
                                            .h(px(GRID_ROW_HEIGHT))
                                            .px(px(GRID_PADDING_X))
                                            .py_2();

                                        for col_ix in 0..cols {
                                            let item_ix = row_ix * cols + col_ix;
                                            if item_ix == 0 {
                                                row = row.child(Self::render_open_file_card(
                                                    this.item_state(item_ix),
                                                    theme,
                                                    cx,
                                                ));
                                            } else if item_ix < item_count {
                                                let entry = this
                                                    .entry_for_item(item_ix)
                                                    .expect("document item must have an entry")
                                                    .clone();
                                                let thumbnail = this
                                                    .thumbnails
                                                    .get(&entry.path)
                                                    .cloned()
                                                    .unwrap_or(ThumbnailState::Loading);
                                                row = row.child(Self::render_document_card(
                                                    &entry,
                                                    NewTabCardState {
                                                        thumbnail,
                                                        item: this.item_state(item_ix),
                                                    },
                                                    theme,
                                                    page_color,
                                                    cx,
                                                ));
                                            } else {
                                                row = row.child(
                                                    div()
                                                        .flex_none()
                                                        .w(px(CARD_WIDTH))
                                                        .h(px(CARD_HEIGHT)),
                                                );
                                            }
                                        }

                                        row.into_any_element()
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar_for_with_id(&scroll, "new-tab-scrollbar", window, cx),
            )
            .into_any_element()
    }

    fn render_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let item_count = self.item_count();
        let rows = item_count + usize::from(self.entries.is_empty());
        let show_location = !self.viewport_width.is_finite()
            || self.viewport_width <= 0.0
            || self.viewport_width >= DETAILS_LAST_OPENED_BREAKPOINT;
        let scroll = self.scroll.clone();

        div()
            .id("new-tab-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(
                div()
                    .relative()
                    .size_full()
                    .child(
                        uniform_list(
                            "new-tab-compact-list",
                            rows,
                            cx.processor(move |this, visible_range: Range<usize>, _, cx| {
                                let theme = cx.theme().colors;
                                visible_range
                                    .map(|item_ix| {
                                        if item_ix >= item_count {
                                            return Self::render_empty_hint(LIST_ROW_HEIGHT, theme);
                                        }

                                        let state = this.item_state(item_ix);
                                        if item_ix == 0 {
                                            Self::render_open_file_row(
                                                state,
                                                false,
                                                DetailsColumns::for_width(this.viewport_width),
                                                theme,
                                                cx,
                                            )
                                        } else {
                                            let entry = this
                                                .entry_for_item(item_ix)
                                                .expect("document item must have an entry")
                                                .clone();
                                            Self::render_document_row(
                                                &entry,
                                                state,
                                                false,
                                                DetailsColumns::for_width(this.viewport_width),
                                                show_location,
                                                theme,
                                                cx,
                                            )
                                        }
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar_for_with_id(&scroll, "new-tab-scrollbar", window, cx),
            )
            .into_any_element()
    }

    fn render_details_header(&self, columns: DetailsColumns, theme: ThemeColor) -> AnyElement {
        let mut header = div()
            .h(px(DETAILS_HEADER_HEIGHT))
            .flex_none()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .border_b(px(1.0))
            .border_color(theme.border)
            .bg(theme.background)
            .text_size(ui_rems(11.0))
            .text_color(theme.muted_foreground);
        if self.selecting {
            header = header.child(div().flex_none().w(px(18.0)));
        }
        header = header.child(div().flex_none().w(ui_rems(16.0))).child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(crate::fl!("new-tab-column-name")),
        );
        if columns.location {
            header = header.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(crate::fl!("new-tab-column-location")),
            );
        }
        if columns.format_size {
            header = header.child(
                div()
                    .flex_none()
                    .w(px(112.0))
                    .truncate()
                    .child(crate::fl!("new-tab-column-format-size")),
            );
        }
        if columns.last_opened {
            header = header.child(
                div()
                    .flex_none()
                    .w(px(140.0))
                    .truncate()
                    .child(crate::fl!("new-tab-column-last-opened")),
            );
        }
        header
            .child(
                div()
                    .flex_none()
                    .w(px(92.0))
                    .truncate()
                    .child(crate::fl!("new-tab-column-progress")),
            )
            .into_any_element()
    }

    fn render_details(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let item_count = self.item_count();
        let rows = item_count + usize::from(self.entries.is_empty());
        let columns = DetailsColumns::for_width(self.viewport_width);
        let scroll = self.scroll.clone();

        div()
            .id("new-tab-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(self.render_details_header(columns, cx.theme().colors))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        uniform_list(
                            "new-tab-details-list",
                            rows,
                            cx.processor(move |this, visible_range: Range<usize>, _, cx| {
                                let theme = cx.theme().colors;
                                visible_range
                                    .map(|item_ix| {
                                        if item_ix >= item_count {
                                            return Self::render_empty_hint(LIST_ROW_HEIGHT, theme);
                                        }

                                        let state = this.item_state(item_ix);
                                        if item_ix == 0 {
                                            Self::render_open_file_row(
                                                state, true, columns, theme, cx,
                                            )
                                        } else {
                                            let entry = this
                                                .entry_for_item(item_ix)
                                                .expect("document item must have an entry")
                                                .clone();
                                            Self::render_document_row(
                                                &entry, state, true, columns, false, theme, cx,
                                            )
                                        }
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar_for_with_id(&scroll, "new-tab-scrollbar", window, cx),
            )
            .into_any_element()
    }

    fn open_file(&mut self, cx: &mut Context<Self>) {
        cx.emit(NewTabEvent::OpenFile);
    }

    fn open_document(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        cx.emit(NewTabEvent::OpenDocument(path));
    }
}

impl NewTabEntry {
    fn from_history(entry: &HistoryEntry) -> Self {
        let file_name = entry
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| crate::fl!("untitled"))
            .into();
        let directory = entry
            .path
            .parent()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
            .into();
        let format = entry
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(DocumentFormat::from_extension);
        let format_label = match format {
            Some(DocumentFormat::Pdf) => "PDF",
            Some(DocumentFormat::Epub) => "EPUB",
            None => "—",
        };
        let file_size = fs::metadata(&entry.path)
            .ok()
            .map(|metadata| format_file_size(metadata.len()))
            .unwrap_or_else(|| "—".to_string());
        let format_size = format!("{format_label} · {file_size}").into();
        let last_opened = i64::try_from(entry.opened)
            .ok()
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0))
            .map(|timestamp| {
                timestamp
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "—".to_string())
            .into();
        let (last_read, progress) = entry
            .position
            .semantic
            .as_ref()
            .map(|position| {
                let progress = (position.progress.clamp(0.0, 1.0) * 100.0).round() as i64;
                (
                    crate::fl!("new-tab-last-read-progress", progress = progress).into(),
                    format!("{progress}%").into(),
                )
            })
            .unwrap_or_else(|| {
                (
                    crate::fl!("new-tab-last-read", page = entry.position.page as i64).into(),
                    crate::fl!("new-tab-progress-page", page = entry.position.page as i64).into(),
                )
            });

        Self {
            path: entry.path.clone(),
            file_name,
            directory,
            last_read,
            progress,
            format_size,
            last_opened,
            format,
        }
    }
}

fn format_file_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

impl EventEmitter<NewTabEvent> for NewTab {}

impl Render for NewTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let view_mode = self.view_mode(cx);
        let measure_view = cx.entity();

        let measure = canvas(
            {
                let measure_view = measure_view.clone();
                move |bounds, _window, cx| {
                    let width = bounds.size.width.as_f32();
                    measure_view.update(cx, |this, cx| this.update_viewport(width, cx));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let content = match view_mode {
            NewTabViewMode::Grid => self.render_grid(window, cx),
            NewTabViewMode::List => self.render_list(window, cx),
            NewTabViewMode::Details => self.render_details(window, cx),
        };

        let selecting = self.selecting;
        let has_selection = !self.selection.is_empty();
        let selected_count = self.selection.len() as i64;
        let library_error = cx
            .global::<LibraryStore>()
            .startup_error()
            .map(|error| format!("Library unavailable. Changes will not be saved: {error}"));

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(px(NEW_TAB_HEADER_HEIGHT))
            .px_3()
            .border_b(px(1.0))
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(ui_rems(13.0))
                    .child(crate::fl!("new-tab-recent-pdfs"))
                    .when(selecting, |this| {
                        this.child(
                            div()
                                .text_size(ui_rems(11.0))
                                .text_color(theme.muted_foreground)
                                .child(crate::fl!("selected-count", count = selected_count)),
                        )
                    }),
            )
            .child({
                let mut toolbar = div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(Self::render_view_button(
                        NewTabViewMode::Grid,
                        view_mode,
                        cx,
                    ))
                    .child(Self::render_view_button(
                        NewTabViewMode::List,
                        view_mode,
                        cx,
                    ))
                    .child(Self::render_view_button(
                        NewTabViewMode::Details,
                        view_mode,
                        cx,
                    ))
                    .child(div().mx_1().h(px(18.0)).w(px(1.0)).bg(theme.border));
                if selecting && has_selection {
                    toolbar = toolbar
                        .child(Self::render_toolbar_button(
                            crate::fl!("new-tab-remove-from-recents"),
                            |this, _, cx| this.delete_selected(cx),
                            cx,
                        ))
                        .child(Self::render_toolbar_button(
                            crate::fl!("cancel-selection"),
                            |this, _, cx| this.cancel_select(cx),
                            cx,
                        ));
                } else if selecting {
                    toolbar = toolbar.child(Self::render_toolbar_button(
                        crate::fl!("cancel"),
                        |this, _, cx| this.cancel_select(cx),
                        cx,
                    ));
                } else {
                    toolbar = toolbar.child(Self::render_toolbar_button(
                        crate::fl!("select"),
                        |this, _, cx| this.enter_select(cx),
                        cx,
                    ));
                }
                toolbar
            });

        div()
            .id("new-tab")
            .relative()
            .size_full()
            .key_context("NewTab")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &ScrollLineDown, _, cx| {
                let mode = this.view_mode(cx);
                this.scroll_by(px(Self::row_height(mode)), cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollLineUp, _, cx| {
                let mode = this.view_mode(cx);
                this.scroll_by(-px(Self::row_height(mode)), cx);
            }))
            .on_action(cx.listener(|this, _: &NextHalfPage, _, cx| {
                let viewport_height = this.scroll.0.borrow().base_handle.bounds().size.height;
                this.scroll_by(viewport_height * 0.5, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousHalfPage, _, cx| {
                let viewport_height = this.scroll.0.borrow().base_handle.bounds().size.height;
                this.scroll_by(-(viewport_height * 0.5), cx);
            }))
            .on_action(cx.listener(|this, _: &EnterSelect, _, cx| {
                this.enter_select(cx);
            }))
            .on_action(cx.listener(|this, _: &CancelSelect, _, cx| {
                this.cancel_select(cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteSelected, _, cx| {
                this.delete_selected(cx);
            }))
            .on_action(cx.listener(|this, _: &CursorUp, _, cx| {
                this.move_cursor(CursorMove::Up, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorDown, _, cx| {
                this.move_cursor(CursorMove::Down, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorLeft, _, cx| {
                this.move_cursor(CursorMove::Left, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorRight, _, cx| {
                this.move_cursor(CursorMove::Right, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorFirst, _, cx| {
                this.move_cursor(CursorMove::First, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorLast, _, cx| {
                this.move_cursor(CursorMove::Last, false, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionUp, _, cx| {
                this.move_cursor(CursorMove::Up, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionDown, _, cx| {
                this.move_cursor(CursorMove::Down, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionLeft, _, cx| {
                this.move_cursor(CursorMove::Left, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionRight, _, cx| {
                this.move_cursor(CursorMove::Right, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionFirst, _, cx| {
                this.move_cursor(CursorMove::First, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionLast, _, cx| {
                this.move_cursor(CursorMove::Last, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivateFocused, _, cx| {
                this.activate_focused(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFocused, _, cx| {
                this.toggle_focused(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.select_all(cx);
            }))
            .flex()
            .flex_col()
            .bg(theme.tab_active)
            .text_size(ui_rems(12.0))
            .text_color(theme.foreground)
            .child(measure)
            .when_some(library_error, |this, error| {
                this.child(
                    div()
                        .mx_3()
                        .mt_3()
                        .p_2()
                        .rounded(px(4.0))
                        .bg(theme.list_hover)
                        .text_color(theme.primary)
                        .child(error),
                )
            })
            .child(header)
            .child(content)
    }
}

fn thumbnail_cache_dir() -> PathBuf {
    glyphst_core::paths::state_dir()
        .join("thumbnails")
        .join(THUMBNAIL_CACHE_VERSION)
}

fn load_or_render_cover_thumbnail(path: &Path, key: &ThumbnailCacheKey) -> Result<ThumbnailImage> {
    let cache_path = key.disk_path();
    if cache_path.is_file()
        && let Ok(image) = image::open(&cache_path)
    {
        let rgba = image.into_rgba8();
        let width = rgba.width() as f32;
        let height = rgba.height() as f32;
        return Ok(ThumbnailImage {
            image: Arc::new(RenderImage::new(smallvec![Frame::new(rgba)])),
            width,
            height,
        });
    }
    render_cover_thumbnail(path, Some(&cache_path))
}

fn persist_cover_thumbnail(path: &Path, image: &RgbaImage) -> Result<()> {
    let Some(parent) = path.parent() else {
        bail!("thumbnail cache path has no parent");
    };
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create thumbnail cache {}", parent.display()))?;
    image
        .save_with_format(path, ImageFormat::Png)
        .with_context(|| format!("failed to save thumbnail cache {}", path.display()))?;

    let mut entries = fs::read_dir(parent)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata
                .is_file()
                .then(|| (metadata.modified().unwrap_or(UNIX_EPOCH), entry.path()))
        })
        .collect::<Vec<_>>();
    if entries.len() > MAX_PERSISTENT_THUMBNAILS {
        entries.sort_by_key(|(modified, _)| *modified);
        let remove_count = entries.len() - MAX_PERSISTENT_THUMBNAILS;
        for (_, path) in entries.into_iter().take(remove_count) {
            if let Err(error) = fs::remove_file(&path) {
                eprintln!(
                    "failed to prune cached new tab preview {}: {error:#}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn render_cover_thumbnail(path: &Path, cache_path: Option<&Path>) -> Result<ThumbnailImage> {
    let mut document = mupdf::Document::open(path)?;
    if document.is_reflowable()? {
        document.layout(480.0, 640.0, 16.0)?;
    }
    let page_count = document.page_count()?;
    if page_count <= 0 {
        bail!("document has no readable content");
    }

    let page = document.load_page(0)?;
    let page_bounds = page.bounds()?;
    let page_width = page_bounds.x1 - page_bounds.x0;
    let page_height = page_bounds.y1 - page_bounds.y0;
    if page_width <= 0.0 || page_height <= 0.0 {
        bail!("PDF first page has invalid bounds");
    }

    let scale = (THUMBNAIL_MAX_WIDTH / page_width)
        .min(THUMBNAIL_MAX_HEIGHT / page_height)
        .max(0.001);
    let logical_width = page_width * scale;
    let logical_height = page_height * scale;
    let physical_width = logical_width.ceil().max(1.0) as i32;
    let physical_height = logical_height.ceil().max(1.0) as i32;

    let display_list = DisplayList::new(page_bounds)?;
    let device = Device::from_display_list(&display_list)?;
    page.run(&device, &Matrix::IDENTITY)?;

    let mut pixmap = Pixmap::new_with_w_h(
        &Colorspace::device_rgb(),
        physical_width,
        physical_height,
        true,
    )?;
    pixmap.samples_mut().fill(255);

    let zoom_x = physical_width as f32 / page_width;
    let zoom_y = physical_height as f32 / page_height;
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
            .context("failed to render PDF preview")?;
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

    let rgba = RgbaImage::from_raw(width, height, bgra)
        .context("failed to create preview image from pixmap")?;
    if let Some(cache_path) = cache_path
        && let Err(error) = persist_cover_thumbnail(cache_path, &rgba)
    {
        eprintln!(
            "failed to cache new tab preview for {}: {error:#}",
            path.display()
        );
    }
    let image = Arc::new(RenderImage::new(smallvec![Frame::new(rgba)]));

    Ok(ThumbnailImage {
        image,
        width: logical_width,
        height: logical_height,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs::{self, OpenOptions},
        io::Write as _,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use gpui::{AppContext as _, TestAppContext};
    use gpui_component::Root;
    use image::Rgba;

    use super::{
        CursorMove, DetailsColumns, NewTab, NewTabEntry, NewTabItemKey, ThumbnailCacheKey,
        format_file_size, persist_cover_thumbnail,
    };
    use crate::{
        library::{DocumentPosition, HistoryEntry, LibraryStore},
        settings::{NewTabViewMode, SettingsStore},
    };

    fn temporary_directory(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "glyphst-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn test_entry(path: &str) -> NewTabEntry {
        NewTabEntry {
            path: PathBuf::from(path),
            file_name: path.into(),
            directory: "/tmp".into(),
            last_read: "Last read".into(),
            progress: "50%".into(),
            format_size: "PDF · 1 KiB".into(),
            last_opened: "2026-07-28 12:00".into(),
            format: Some(glyphst_core::DocumentFormat::Pdf),
        }
    }

    fn history_entry(path: PathBuf, opened: u64) -> HistoryEntry {
        HistoryEntry {
            path,
            opened,
            position: DocumentPosition::fixed(0.0, 0.0, 1.0, 1),
        }
    }

    #[test]
    fn details_columns_hide_in_priority_order() {
        assert_eq!(
            DetailsColumns::for_width(900.0),
            DetailsColumns {
                location: true,
                format_size: true,
                last_opened: true,
            }
        );
        assert_eq!(
            DetailsColumns::for_width(700.0),
            DetailsColumns {
                location: false,
                format_size: true,
                last_opened: true,
            }
        );
        assert_eq!(
            DetailsColumns::for_width(550.0),
            DetailsColumns {
                location: false,
                format_size: false,
                last_opened: true,
            }
        );
        assert_eq!(
            DetailsColumns::for_width(400.0),
            DetailsColumns {
                location: false,
                format_size: false,
                last_opened: false,
            }
        );
    }

    #[test]
    fn selection_ranges_include_documents_but_not_open_file() {
        let entries = vec![
            test_entry("/tmp/a.pdf"),
            test_entry("/tmp/b.pdf"),
            test_entry("/tmp/c.pdf"),
        ];

        assert_eq!(
            NewTab::selection_range(&entries, PathBuf::from("/tmp/b.pdf").as_path(), 0),
            BTreeSet::from([PathBuf::from("/tmp/a.pdf"), PathBuf::from("/tmp/b.pdf"),])
        );
        assert_eq!(
            NewTab::selection_range(&entries, PathBuf::from("/tmp/a.pdf").as_path(), 3),
            BTreeSet::from([
                PathBuf::from("/tmp/a.pdf"),
                PathBuf::from("/tmp/b.pdf"),
                PathBuf::from("/tmp/c.pdf"),
            ])
        );
    }

    #[test]
    fn file_sizes_use_binary_units() {
        assert_eq!(format_file_size(42), "42 B");
        assert_eq!(format_file_size(1536), "1.5 KiB");
        assert_eq!(format_file_size(2 * 1024 * 1024), "2.0 MiB");
    }

    #[gpui::test]
    fn open_file_is_first_in_all_three_rendered_modes(cx: &mut TestAppContext) {
        let directory = temporary_directory("new-tab-view-modes");
        fs::create_dir(&directory).unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            let mut settings = SettingsStore::memory();
            settings.state_path = directory.join("settings.kdl");
            cx.set_global(settings);
            cx.set_global(LibraryStore::memory());
        });

        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new(cx.focus_handle(), cx));
            Root::new(view, window, cx)
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("new-tab-open-file-grid").is_some());
        assert!(cx.debug_bounds("new-tab-view-grid").is_some());
        assert!(cx.debug_bounds("new-tab-view-list").is_some());
        assert!(cx.debug_bounds("new-tab-view-details").is_some());

        let list = cx.debug_bounds("new-tab-view-list").unwrap().center();
        cx.simulate_click(list, gpui::Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("new-tab-open-file-list").is_some());

        let details = cx.debug_bounds("new-tab-view-details").unwrap().center();
        cx.simulate_click(details, gpui::Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
            assert_eq!(
                cx.global::<SettingsStore>().settings.new_tab.view_mode,
                NewTabViewMode::Details
            );
        });
        assert!(cx.debug_bounds("new-tab-open-file-details").is_some());

        fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn keyboard_selection_supports_ranges_and_select_all(cx: &mut TestAppContext) {
        let directory = temporary_directory("new-tab-selection");
        fs::create_dir(&directory).unwrap();
        let paths = ["a.pdf", "b.pdf", "c.pdf"]
            .into_iter()
            .map(|name| directory.join(name))
            .collect::<Vec<_>>();
        for path in &paths {
            fs::write(path, b"document").unwrap();
        }

        cx.update(|cx| {
            gpui_component::init(cx);
            let mut settings_store = SettingsStore::memory();
            let mut settings = settings_store.settings.clone();
            settings.new_tab.view_mode = NewTabViewMode::List;
            settings_store.set_settings_in_memory(settings);
            cx.set_global(settings_store);

            let mut library = LibraryStore::memory();
            library.library.history = paths
                .iter()
                .enumerate()
                .map(|(ix, path)| history_entry(path.clone(), (10 - ix) as u64))
                .collect();
            cx.set_global(library);
        });

        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new(cx.focus_handle(), cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });

        view.update_in(cx, |this, _, cx| {
            assert_eq!(this.item_key(0), Some(NewTabItemKey::OpenFile));

            this.handle_document_pointer(paths[0].clone(), gpui::Modifiers::secondary_key(), cx);
            assert_eq!(this.selection, BTreeSet::from([paths[0].clone()]));
            this.handle_document_pointer(
                paths[2].clone(),
                gpui::Modifiers {
                    shift: true,
                    ..Default::default()
                },
                cx,
            );
            assert_eq!(this.selection.len(), 3);
            this.cancel_select(cx);
            this.focused_item = NewTabItemKey::OpenFile;

            this.move_cursor(CursorMove::Down, false, cx);
            this.toggle_focused(cx);
            assert_eq!(this.selection.len(), 1);

            this.move_cursor(CursorMove::Down, true, cx);
            assert_eq!(
                this.selection,
                BTreeSet::from([paths[0].clone(), paths[1].clone()])
            );

            this.select_all(cx);
            assert_eq!(this.selection.len(), 3);
            this.cancel_select(cx);
            assert!(this.selection.is_empty());
            assert!(!this.selecting);
            assert_eq!(cx.global::<LibraryStore>().history_entries().len(), 3);
        });

        for path in &paths {
            assert!(path.is_file(), "removing history must not delete {path:?}");
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn thumbnail_fingerprint_changes_when_the_document_changes() {
        let directory = temporary_directory("thumbnail-fingerprint");
        fs::create_dir(&directory).unwrap();
        let document = directory.join("document.pdf");
        fs::write(&document, b"first").unwrap();
        let first = ThumbnailCacheKey::for_path(&document).unwrap();

        OpenOptions::new()
            .append(true)
            .open(&document)
            .unwrap()
            .write_all(b"-changed")
            .unwrap();
        let changed = ThumbnailCacheKey::for_path(&document).unwrap();

        assert_ne!(first, changed);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn persistent_thumbnail_round_trips_png_pixels() {
        let directory = temporary_directory("thumbnail-persistence");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("thumbnail.png");
        let image = image::RgbaImage::from_pixel(3, 2, Rgba([10, 20, 30, 255]));

        persist_cover_thumbnail(&path, &image).unwrap();
        let restored = image::open(&path).unwrap().into_rgba8();

        assert_eq!(restored, image);
        fs::remove_dir_all(directory).unwrap();
    }
}
