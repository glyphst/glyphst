use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{
    AnyElement, Context, Corners, EventEmitter, FocusHandle, IntoElement, MouseButton, Pixels,
    Render, RenderImage, SharedString, UniformListScrollHandle, Window, canvas, div, fill, point,
    prelude::*, px, rgb, rgba, uniform_list,
};
use image::{Frame, RgbaImage};
use mupdf::{Colorspace, Device, DisplayList, Matrix, Pixmap, Rect};
use smallvec::smallvec;

use crate::{
    library::{HistoryEntry, LibraryStore},
    pdf_view::{NextHalfPage, PreviousHalfPage, ScrollLineDown, ScrollLineUp},
    scrollbar::{ScrollableHandle, uniform_list_scrollbar_for},
    settings::{SettingsStore, ui_rems},
    theme::Theme,
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

#[derive(Clone)]
struct ThumbnailImage {
    image: Arc<RenderImage>,
    width: f32,
    height: f32,
}

#[derive(Clone)]
enum ThumbnailState {
    Loading,
    Ready(ThumbnailImage),
    Failed,
}

#[derive(Clone)]
struct NewTabEntry {
    path: PathBuf,
    file_name: SharedString,
    directory: SharedString,
    page: usize,
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
}

impl NewTab {
    pub(crate) const WINDOW_WIDTH: f32 = BOARD_WIDTH;
    pub(crate) const WINDOW_HEIGHT: f32 = BOARD_HEIGHT;

    pub(crate) fn new(focus_handle: FocusHandle, cx: &mut Context<Self>) -> Self {
        let entries = {
            let store = cx.global_mut::<LibraryStore>();
            if store.remove_missing_history_entries()
                && let Err(error) = store.save()
            {
                eprintln!("failed to save pruned PDF history: {error:#}");
            }

            store
                .history_entries()
                .iter()
                .map(NewTabEntry::from_history)
                .collect::<Vec<_>>()
        };

        let mut board = Self {
            entries,
            thumbnails: HashMap::new(),
            focus_handle,
            scroll: UniformListScrollHandle::new(),
            cols: DEFAULT_COLS,
            viewport_width: 0.0,
        };

        for path in board
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>()
        {
            board.start_thumbnail_render(path, cx);
        }

        cx.observe_global::<SettingsStore>(|_, cx| cx.notify())
            .detach();
        board
    }

    fn start_thumbnail_render(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.thumbnails
            .insert(path.clone(), ThumbnailState::Loading);

        cx.spawn(async move |this, cx| {
            let render_path = path.clone();
            let result = cx
                .background_spawn(async move { render_cover_thumbnail(&render_path) })
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
        let max_offset = self.scroll.max_offset().y;
        if max_offset <= px(0.) {
            return;
        }

        let current = self.scroll.offset();
        let new_y = (current.y - delta_y).clamp(-max_offset, px(0.));
        self.scroll.set_offset(point(current.x, new_y));
        cx.notify();
    }

    fn render_card(
        path: PathBuf,
        file_name: SharedString,
        directory: SharedString,
        page: usize,
        thumbnail: ThumbnailState,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .flex_col()
            .items_center()
            .w(px(CARD_WIDTH))
            .h(px(CARD_HEIGHT))
            .p_3()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.open_document(path.clone(), cx);
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
                    .child(Self::render_thumbnail(thumbnail, theme)),
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
                    .text_color(rgb(theme.text_muted))
                    .child(directory),
            )
            .child(
                div()
                    .mt_1()
                    .w_full()
                    .truncate()
                    .text_size(ui_rems(11.0))
                    .text_color(rgb(theme.text_muted))
                    .child(format!("Last read: p. {page}")),
            )
    }

    fn render_thumbnail(thumbnail: ThumbnailState, theme: Theme) -> AnyElement {
        match thumbnail {
            ThumbnailState::Ready(thumbnail) => {
                let image = thumbnail.image.clone();
                canvas(
                    move |_, _, _| image.clone(),
                    move |bounds, image, window, _| {
                        window.paint_quad(fill(bounds, rgb(theme.page)));
                        if let Err(error) =
                            window.paint_image(bounds, Corners::default(), image, 0, false)
                        {
                            eprintln!("failed to paint new tab preview: {error:#}");
                        }
                    },
                )
                .w(px(thumbnail.width))
                .h(px(thumbnail.height))
                .border_1()
                .border_color(rgb(theme.border))
                .into_any_element()
            }
            ThumbnailState::Loading => div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(THUMBNAIL_MAX_WIDTH))
                .h(px(THUMBNAIL_MAX_HEIGHT))
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.page))
                .text_size(ui_rems(11.0))
                .text_color(rgb(theme.text_muted))
                .child("Rendering preview…")
                .into_any_element(),
            ThumbnailState::Failed => div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(THUMBNAIL_MAX_WIDTH))
                .h(px(THUMBNAIL_MAX_HEIGHT))
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgba(((theme.error & 0x00ff_ffff) << 8) | 0x18))
                .text_size(ui_rems(11.0))
                .text_color(rgb(theme.text_muted))
                .child("Preview unavailable")
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
            .unwrap_or("Untitled")
            .to_string()
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
            page: entry.position.page,
        }
    }
}

impl EventEmitter<NewTabEvent> for NewTab {}

impl Render for NewTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<SettingsStore>().resolved_theme;
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
                .text_color(rgb(theme.text_muted))
                .child(div().text_size(ui_rems(16.0)).child("No recent PDFs"))
                .child(
                    div()
                        .text_size(ui_rems(12.0))
                        .child("Open a PDF to add it to history."),
                )
                .into_any_element()
        } else {
            div()
                .id("new-tab-content")
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(
                    uniform_list(
                        "new-tab-list",
                        rows,
                        cx.processor(move |this, visible_range: Range<usize>, _window, cx| {
                            let theme = cx.global::<SettingsStore>().resolved_theme;

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
                                        if let Some(entry) = this.entries.get(entry_ix) {
                                            let path = entry.path.clone();
                                            let file_name = entry.file_name.clone();
                                            let directory = entry.directory.clone();
                                            let page = entry.page;
                                            let thumbnail = this
                                                .thumbnails
                                                .get(&path)
                                                .cloned()
                                                .unwrap_or(ThumbnailState::Loading);

                                            row = row.child(Self::render_card(
                                                path, file_name, directory, page, thumbnail, theme,
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
                        }),
                    )
                    .track_scroll(&scroll_handle)
                    .with_decoration(uniform_list_scrollbar_for(
                        &scroll_handle,
                        "new-tab-scrollbar",
                        window,
                        cx,
                    ))
                    .size_full(),
                )
                .into_any_element()
        };

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
                this.scroll_by(this.scroll.viewport().size.height * 0.5, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousHalfPage, _, cx| {
                this.scroll_by(-(this.scroll.viewport().size.height * 0.5), cx);
            }))
            .flex()
            .flex_col()
            .bg(rgb(theme.tab_active_background))
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.text))
            .child(measure)
            .child(content)
    }
}

fn render_cover_thumbnail(path: &Path) -> Result<ThumbnailImage> {
    let document = mupdf::Document::open(
        path.to_str()
            .ok_or_else(|| anyhow!("PDF path is not valid UTF-8"))?,
    )?;
    let page_count = document.page_count()?;
    if page_count <= 0 {
        bail!("PDF has no pages");
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

    let image = Arc::new(RenderImage::new(smallvec![Frame::new(
        RgbaImage::from_raw(width, height, bgra)
            .context("failed to create preview image from pixmap")?,
    )]));

    Ok(ThumbnailImage {
        image,
        width: logical_width,
        height: logical_height,
    })
}
