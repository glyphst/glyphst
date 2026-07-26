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
use gpui::{
    AnyElement, Context, Corners, EventEmitter, FocusHandle, IntoElement, MouseButton, Pixels,
    Render, RenderImage, SharedString, UniformListScrollHandle, Window, canvas, div, fill, point,
    prelude::*, px, rgb, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, ThemeColor, alert::Alert, button::Button, checkbox::Checkbox,
};
use image::{Frame, ImageFormat, RgbaImage};
use mupdf::{Colorspace, Device, DisplayList, Matrix, Pixmap, Rect};
use smallvec::smallvec;

pub use glyphst_actions::new_tab::{CancelSelect, DeleteSelected, EnterSelect};

use crate::{
    library::{HistoryEntry, LibraryStore},
    pdf_view::{NextHalfPage, PreviousHalfPage, ScrollLineDown, ScrollLineUp},
    scrollbar::{Scrollbar, ScrollbarHandle as _},
    settings::{SettingsStore, ui_rems},
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
const NEW_TAB_ROW_HEIGHT: f32 = CARD_HEIGHT + GRID_GAP;
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

struct NewTabCardState {
    thumbnail: ThumbnailState,
    selecting: bool,
    selected: bool,
    entry_ix: usize,
}

#[derive(Clone)]
struct NewTabEntry {
    path: PathBuf,
    file_name: SharedString,
    directory: SharedString,
    last_read: SharedString,
}

pub(crate) enum NewTabEvent {
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
    selection: BTreeSet<usize>,
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

        let board = Self {
            entries,
            thumbnails: HashMap::new(),
            focus_handle,
            scroll: UniformListScrollHandle::new(),
            cols: DEFAULT_COLS,
            viewport_width: 0.0,
            selecting: false,
            selection: BTreeSet::new(),
        };

        cx.observe_global::<SettingsStore>(|_, cx| cx.notify())
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

    fn update_cols(&mut self, viewport_width: f32, cx: &mut Context<Self>) {
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
        cx.notify();
    }

    fn cancel_select(&mut self, cx: &mut Context<Self>) {
        let changed = self.selecting || !self.selection.is_empty();
        self.selecting = false;
        self.selection.clear();
        if changed {
            cx.notify();
        }
    }

    fn toggle_selection(&mut self, entry_ix: usize, cx: &mut Context<Self>) {
        if !self.selection.insert(entry_ix) {
            self.selection.remove(&entry_ix);
        }
        cx.notify();
    }

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            return;
        }

        let paths: Vec<PathBuf> = self
            .selection
            .iter()
            .filter_map(|&ix| self.entries.get(ix).map(|entry| entry.path.clone()))
            .collect();

        if paths.is_empty() {
            self.selection.clear();
            cx.notify();
            return;
        }

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
        self.selection.clear();
        cx.notify();
    }

    fn refresh_entries(&mut self, cx: &mut Context<Self>) {
        let store = cx.global::<LibraryStore>();
        self.entries = store
            .history_entries()
            .iter()
            .map(NewTabEntry::from_history)
            .collect();
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

    fn render_card(
        entry: &NewTabEntry,
        state: NewTabCardState,
        theme: ThemeColor,
        page_color: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let NewTabCardState {
            thumbnail,
            selecting,
            selected,
            entry_ix,
        } = state;
        let path = entry.path.clone();
        let file_name = entry.file_name.clone();
        let directory = entry.directory.clone();
        let last_read = entry.last_read.clone();
        let border_color = if selected {
            theme.primary
        } else {
            theme.border
        };
        let card_bg = if selected {
            theme.primary.opacity(0x22 as f32 / 255.0)
        } else {
            theme.popover
        };

        div()
            .id(("new-tab-card", entry_ix))
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
                cx.listener(move |this, _, _, cx| {
                    if this.selecting {
                        this.toggle_selection(entry_ix, cx);
                    } else {
                        this.open_document(path.clone(), cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .when(selecting, |card| {
                card.child(
                    Checkbox::new(("new-tab-card-selection", entry_ix))
                        .xsmall()
                        .checked(selected)
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

    fn open_document(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        cx.emit(NewTabEvent::OpenDocument(path));
    }
}

impl NewTabEntry {
    fn from_history(entry: &HistoryEntry) -> Self {
        let file_name = entry
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| crate::fl!("untitled"))
            .into();
        let directory = entry
            .path
            .parent()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
            .into();

        Self {
            path: entry.path.clone(),
            file_name,
            directory,
            last_read: entry
                .position
                .semantic
                .as_ref()
                .map(|position| {
                    crate::fl!(
                        "new-tab-last-read-progress",
                        progress = (position.progress.clamp(0.0, 1.0) * 100.0).round() as i64
                    )
                    .into()
                })
                .unwrap_or_else(|| {
                    crate::fl!("new-tab-last-read", page = entry.position.page as i64).into()
                }),
        }
    }
}

impl EventEmitter<NewTabEvent> for NewTab {}

impl Render for NewTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let cols = self.cols.max(1);
        let rows = self.entries.len().div_ceil(cols);
        let horizontal_gap = Self::horizontal_gap(self.viewport_width, cols);
        let scroll_handle = self.scroll.clone();
        let measure_view = cx.entity();

        let measure = canvas(
            {
                let measure_view = measure_view.clone();
                move |bounds, _window, cx| {
                    let width = bounds.size.width.as_f32();
                    measure_view.update(cx, |this, cx| this.update_cols(width, cx));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let content = if self.entries.is_empty() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .text_size(ui_rems(16.0))
                        .child(crate::fl!("new-tab-empty-title")),
                )
                .child(
                    div()
                        .text_size(ui_rems(12.0))
                        .child(crate::fl!("new-tab-empty-description")),
                )
                .into_any_element()
        } else {
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
                                "new-tab-list",
                                rows,
                                cx.processor(
                                    move |this, visible_range: Range<usize>, _window, cx| {
                                        let document_theme =
                                            cx.global::<SettingsStore>().resolved_theme;
                                        let theme = cx.theme().colors;
                                        let prewarm_range = visible_range.start.saturating_sub(1)
                                            ..(visible_range.end + 1).min(rows);
                                        let visible_paths = prewarm_range
                                            .flat_map(|row_ix| {
                                                let first_entry_ix = row_ix * cols;
                                                first_entry_ix..first_entry_ix + cols
                                            })
                                            .filter_map(|entry_ix| {
                                                this.entries
                                                    .get(entry_ix)
                                                    .map(|entry| entry.path.clone())
                                            })
                                            .collect::<Vec<_>>();

                                        for path in visible_paths {
                                            this.start_thumbnail_render(path, cx);
                                        }

                                        visible_range
                                            .map(|row_ix| {
                                                let mut row = div()
                                                    .flex()
                                                    .flex_row()
                                                    .items_start()
                                                    .justify_center()
                                                    .gap(px(horizontal_gap))
                                                    .w_full()
                                                    .h(px(CARD_HEIGHT + GRID_GAP))
                                                    .px(px(GRID_PADDING_X))
                                                    .py_2();

                                                for col_ix in 0..cols {
                                                    let entry_ix = row_ix * cols + col_ix;
                                                    if let Some(entry) = this.entries.get(entry_ix)
                                                    {
                                                        let thumbnail = this
                                                            .thumbnails
                                                            .get(&entry.path)
                                                            .cloned()
                                                            .unwrap_or(ThumbnailState::Loading);
                                                        let selecting = this.selecting;
                                                        let selected =
                                                            this.selection.contains(&entry_ix);

                                                        row = row.child(Self::render_card(
                                                            entry,
                                                            NewTabCardState {
                                                                thumbnail,
                                                                selecting,
                                                                selected,
                                                                entry_ix,
                                                            },
                                                            theme,
                                                            document_theme.page,
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

                                                row
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                ),
                            )
                            .track_scroll(&scroll_handle)
                            .size_full(),
                        )
                        .child(Scrollbar::vertical(&scroll_handle).id("new-tab-scrollbar")),
                )
                .into_any_element()
        };

        let selecting = self.selecting;
        let has_selection = !self.selection.is_empty();
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
                    .text_size(ui_rems(13.0))
                    .child(crate::fl!("new-tab-recent-pdfs")),
            )
            .child({
                let mut toolbar = div().flex().items_center().gap_2();
                if selecting && has_selection {
                    toolbar = toolbar
                        .child(Self::render_toolbar_button(
                            crate::fl!("delete"),
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
                this.scroll_by(px(NEW_TAB_ROW_HEIGHT), cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollLineUp, _, cx| {
                this.scroll_by(-px(NEW_TAB_ROW_HEIGHT), cx);
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
        fs::{self, OpenOptions},
        io::Write as _,
        time::{SystemTime, UNIX_EPOCH},
    };

    use image::Rgba;

    use super::{ThumbnailCacheKey, persist_cover_thumbnail};

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
