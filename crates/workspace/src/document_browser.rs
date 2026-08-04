use std::{
    collections::HashSet,
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use glyphst_core::DocumentFormat;
use gpui::{
    AnyElement, App, Context, DragMoveEvent, Empty, EventEmitter, FocusHandle, Focusable,
    IntoElement, KeystrokeEvent, MouseButton, MouseDownEvent, Pixels, Point, Render,
    ScrollStrategy, SharedString, Subscription, Task, UniformListScrollHandle, Window, div,
    prelude::*, px, relative, transparent_black, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, FocusTrapElement as _, IconName as ComponentIconName, Sizable as _,
    StyledExt as _, ThemeColor,
    button::Button,
    input::{Input, InputEvent, InputState},
    skeleton::Skeleton,
};

use crate::{
    icons::{Icon, IconName},
    library::{DocumentBrowserSize, LibraryStore},
    scrollbar::WithScrollbar as _,
    settings::ui_rems,
};

const BROWSER_MIN_WIDTH: f32 = 680.0;
const BROWSER_MIN_HEIGHT: f32 = 415.0;
const BROWSER_VIEWPORT_MARGIN: f32 = 16.0;
const RESIZE_EDGE_SIZE: f32 = 8.0;
const RESIZE_CORNER_SIZE: f32 = 16.0;
const PLACES_WIDTH: f32 = 196.0;
const ROW_HEIGHT: f32 = 32.0;
const MAX_RECENT_PLACES: usize = 5;

fn default_browser_size() -> DocumentBrowserSize {
    DocumentBrowserSize {
        width: BROWSER_MIN_WIDTH,
        height: BROWSER_MIN_HEIGHT,
    }
}

fn sanitize_preferred_size(size: DocumentBrowserSize) -> DocumentBrowserSize {
    let default = default_browser_size();
    DocumentBrowserSize {
        width: if size.width.is_finite() {
            size.width.max(BROWSER_MIN_WIDTH)
        } else {
            default.width
        },
        height: if size.height.is_finite() {
            size.height.max(BROWSER_MIN_HEIGHT)
        } else {
            default.height
        },
    }
}

fn available_browser_size(viewport_width: f32, viewport_height: f32) -> DocumentBrowserSize {
    DocumentBrowserSize {
        width: (viewport_width - BROWSER_VIEWPORT_MARGIN * 2.0).max(0.0),
        height: (viewport_height - BROWSER_VIEWPORT_MARGIN * 2.0).max(0.0),
    }
}

fn fit_browser_size(
    preferred: DocumentBrowserSize,
    viewport_width: f32,
    viewport_height: f32,
) -> DocumentBrowserSize {
    // Clamp only the rendered size so a temporary compositor constraint does not
    // overwrite the user's preferred dimensions.
    let preferred = sanitize_preferred_size(preferred);
    let available = available_browser_size(viewport_width, viewport_height);
    DocumentBrowserSize {
        width: preferred.width.min(available.width),
        height: preferred.height.min(available.height),
    }
}

fn clamp_resized_dimension(candidate: f32, minimum: f32, available: f32) -> f32 {
    candidate.max(minimum).min(available.max(minimum))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    Directory,
    Pdf,
    Epub,
}

impl EntryKind {
    fn label(self) -> String {
        match self {
            Self::Directory => crate::fl!("document-browser-folder-kind"),
            Self::Pdf => "PDF".to_string(),
            Self::Epub => "EPUB".to_string(),
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Directory => IconName::FolderOpen,
            Self::Pdf => IconName::FileText,
            Self::Epub => IconName::BookOpen,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BrowserEntry {
    path: PathBuf,
    name: SharedString,
    kind: EntryKind,
}

#[derive(Clone)]
struct Place {
    label: SharedString,
    path: PathBuf,
}

#[derive(Clone, Debug)]
enum LoadState {
    Loading,
    Ready,
    Error(SharedString),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeAxis {
    Width,
    Height,
    Both,
}

impl ResizeAxis {
    fn includes_width(self) -> bool {
        matches!(self, Self::Width | Self::Both)
    }

    fn includes_height(self) -> bool {
        matches!(self, Self::Height | Self::Both)
    }
}

#[derive(Clone)]
struct DraggedDocumentBrowser(ResizeAxis);

impl Render for DraggedDocumentBrowser {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[derive(Clone, Copy, Debug)]
struct ResizeDrag {
    axis: ResizeAxis,
    initial_pointer: Point<Pixels>,
    initial_actual_size: DocumentBrowserSize,
    changed: bool,
}

#[derive(Clone, Debug)]
pub enum DocumentBrowserEvent {
    Open(PathBuf),
    Dismissed,
}

pub struct DocumentBrowser {
    input: gpui::Entity<InputState>,
    modal_focus_handle: FocusHandle,
    preferred_size: DocumentBrowserSize,
    resize_drag: Option<ResizeDrag>,
    current_dir: PathBuf,
    places: Vec<Place>,
    entries: Vec<BrowserEntry>,
    filtered: Vec<usize>,
    selected: usize,
    show_hidden: bool,
    load_state: LoadState,
    load_generation: u64,
    scroll: UniformListScrollHandle,
    load_task: Option<Task<()>>,
    _input_subscription: Subscription,
    _key_interceptor: Subscription,
}

impl DocumentBrowser {
    pub fn suggested_directory(active_document: Option<&Path>, cx: &App) -> PathBuf {
        active_document
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| {
                cx.global::<LibraryStore>()
                    .history_entries()
                    .first()
                    .and_then(|entry| entry.path.parent())
                    .map(Path::to_path_buf)
            })
            .or_else(dirs::document_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn new(initial_dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::fl!("document-browser-filter-placeholder"))
        });
        let _input_subscription = cx.subscribe_in(&input, window, Self::on_input_event);
        let key_listener = cx.listener(Self::on_keystroke);
        let _key_interceptor = cx.intercept_keystrokes(key_listener);
        let preferred_size = cx
            .global::<LibraryStore>()
            .document_browser_size()
            .map(sanitize_preferred_size)
            .unwrap_or_else(default_browser_size);
        let mut browser = Self {
            input: input.clone(),
            modal_focus_handle: cx.focus_handle(),
            preferred_size,
            resize_drag: None,
            current_dir: initial_dir.clone(),
            places: Self::build_places(cx),
            entries: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            show_hidden: false,
            load_state: LoadState::Loading,
            load_generation: 0,
            scroll: UniformListScrollHandle::new(),
            load_task: None,
            _input_subscription,
            _key_interceptor,
        };
        browser.load_directory(initial_dir, cx);

        let focus = input.focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
        browser
    }

    fn build_places(cx: &App) -> Vec<Place> {
        fn add_place(
            places: &mut Vec<Place>,
            seen: &mut HashSet<PathBuf>,
            label: impl Into<SharedString>,
            path: Option<PathBuf>,
        ) {
            let Some(path) = path else { return };
            if seen.insert(path.clone()) {
                places.push(Place {
                    label: label.into(),
                    path,
                });
            }
        }

        let mut places = Vec::new();
        let mut seen = HashSet::new();
        add_place(
            &mut places,
            &mut seen,
            crate::fl!("document-browser-home"),
            dirs::home_dir(),
        );
        add_place(
            &mut places,
            &mut seen,
            crate::fl!("document-browser-documents"),
            dirs::document_dir(),
        );
        add_place(
            &mut places,
            &mut seen,
            crate::fl!("document-browser-downloads"),
            dirs::download_dir(),
        );

        for entry in cx
            .global::<LibraryStore>()
            .history_entries()
            .iter()
            .take(MAX_RECENT_PLACES * 2)
        {
            let Some(parent) = entry.path.parent().map(Path::to_path_buf) else {
                continue;
            };
            let label = parent
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| parent.display().to_string());
            add_place(&mut places, &mut seen, label, Some(parent));
            if places.len() >= 3 + MAX_RECENT_PLACES {
                break;
            }
        }
        places
    }

    fn read_directory(path: &Path, show_hidden: bool) -> Result<Vec<BrowserEntry>, String> {
        let read_dir = fs::read_dir(path).map_err(|error| {
            crate::fl!(
                "document-browser-read-error",
                path = path.display().to_string(),
                error = error.to_string()
            )
        })?;
        let mut entries = Vec::new();
        for entry in read_dir {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let entry_path = entry.path();
            let Ok(metadata) = fs::metadata(&entry_path) else {
                continue;
            };
            let kind = if metadata.is_dir() {
                EntryKind::Directory
            } else if metadata.is_file() {
                let format = entry_path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .and_then(DocumentFormat::from_extension);
                match format {
                    Some(DocumentFormat::Pdf) => EntryKind::Pdf,
                    Some(DocumentFormat::Epub) => EntryKind::Epub,
                    None => continue,
                }
            } else {
                continue;
            };
            entries.push(BrowserEntry {
                path: entry_path,
                name: name.into(),
                kind,
            });
        }
        Self::sort_entries(&mut entries);
        Ok(entries)
    }

    fn sort_entries(entries: &mut [BrowserEntry]) {
        entries.sort_by(|left, right| {
            let left_group = usize::from(left.kind != EntryKind::Directory);
            let right_group = usize::from(right.kind != EntryKind::Directory);
            left_group.cmp(&right_group).then_with(|| {
                left.name
                    .to_lowercase()
                    .cmp(&right.name.to_lowercase())
                    .then_with(|| left.name.cmp(&right.name))
            })
        });
    }

    fn load_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.current_dir = path.clone();
        self.entries.clear();
        self.filtered.clear();
        self.selected = 0;
        self.load_state = LoadState::Loading;
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        let show_hidden = self.show_hidden;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { Self::read_directory(&path, show_hidden) })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                match result {
                    Ok(entries) => {
                        this.entries = entries;
                        this.load_state = LoadState::Ready;
                        this.update_filter(cx);
                    }
                    Err(error) => {
                        this.load_state = LoadState::Error(error.into());
                        this.entries.clear();
                        this.filtered.clear();
                    }
                }
                this.load_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn update_filter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        self.filtered = Self::filter_indices(&query, &self.entries);
        self.selected = 0;
        if !self.filtered.is_empty() {
            self.scroll
                .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        }
    }

    fn filter_indices(query: &str, entries: &[BrowserEntry]) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        let mut ranked = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                if query.is_empty() {
                    return Some((1, index));
                }
                let name = entry.name.to_lowercase();
                if name.starts_with(&query) {
                    Some((0, index))
                } else if name.contains(&query) {
                    Some((1, index))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|&(rank, index)| (rank, index));
        ranked.into_iter().map(|(_, index)| index).collect()
    }

    fn on_input_event(
        &mut self,
        _: &gpui::Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.update_filter(cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {}
            InputEvent::CompletionAccepted { .. }
            | InputEvent::SpanHoverChanged { .. }
            | InputEvent::Focus
            | InputEvent::Blur => {}
        }
    }

    fn clear_query(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    fn navigate_to(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_query(window, cx);
        self.load_directory(path, cx);
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) else {
            return;
        };
        self.navigate_to(parent, window, cx);
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self
            .filtered
            .get(self.selected)
            .and_then(|index| self.entries.get(*index))
            .cloned()
        else {
            return;
        };
        if entry.kind == EntryKind::Directory {
            self.navigate_to(entry.path, window, cx);
        } else {
            cx.emit(DocumentBrowserEvent::Open(entry.path));
        }
    }

    fn select_next(&mut self, cx: &mut Context<Self>) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.filtered.len();
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn select_prev(&mut self, cx: &mut Context<Self>) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .checked_sub(1)
            .unwrap_or(self.filtered.len() - 1);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn on_keystroke(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.modal_focus_handle.contains_focused(window, cx) {
            return;
        }

        let key = event.keystroke.key.as_str();
        if key == "escape" {
            cx.emit(DocumentBrowserEvent::Dismissed);
            cx.stop_propagation();
            return;
        }

        let navigation_focus = self.input.focus_handle(cx).is_focused(window)
            || self.modal_focus_handle.is_focused(window);
        if !navigation_focus {
            return;
        }

        let handled = match key {
            "up" => {
                self.select_prev(cx);
                true
            }
            "down" => {
                self.select_next(cx);
                true
            }
            "left" => {
                self.go_up(window, cx);
                true
            }
            "right" | "enter" => {
                self.open_selected(window, cx);
                true
            }
            "backspace" if self.input.read(cx).value().is_empty() => {
                self.go_up(window, cx);
                true
            }
            "h" if event.keystroke.modifiers.control => {
                self.show_hidden = !self.show_hidden;
                self.load_directory(self.current_dir.clone(), cx);
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn actual_size(&self, window: &Window) -> DocumentBrowserSize {
        let viewport = window.viewport_size();
        fit_browser_size(
            self.preferred_size,
            viewport.width.as_f32(),
            viewport.height.as_f32(),
        )
    }

    fn begin_resize(
        &mut self,
        axis: ResizeAxis,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_drag = Some(ResizeDrag {
            axis,
            initial_pointer: event.position,
            initial_actual_size: self.actual_size(window),
            changed: false,
        });
        self.modal_focus_handle.focus(window, cx);
        cx.stop_propagation();
    }

    fn resize_from_drag(
        &mut self,
        event: &DragMoveEvent<DraggedDocumentBrowser>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let axis = event.drag(cx).0;
        let Some(mut drag) = self.resize_drag else {
            return;
        };
        if drag.axis != axis {
            return;
        }

        let viewport = window.viewport_size();
        let available = available_browser_size(viewport.width.as_f32(), viewport.height.as_f32());
        // The card remains centered, so each edge moves by half of its size delta.
        let delta_x = (event.event.position.x - drag.initial_pointer.x).as_f32() * 2.0;
        let delta_y = (event.event.position.y - drag.initial_pointer.y).as_f32() * 2.0;
        let previous = self.preferred_size;
        if axis.includes_width() {
            self.preferred_size.width = clamp_resized_dimension(
                drag.initial_actual_size.width + delta_x,
                BROWSER_MIN_WIDTH,
                available.width,
            );
        }
        if axis.includes_height() {
            self.preferred_size.height = clamp_resized_dimension(
                drag.initial_actual_size.height + delta_y,
                BROWSER_MIN_HEIGHT,
                available.height,
            );
        }
        drag.changed |= self.preferred_size != previous;
        self.resize_drag = Some(drag);
        cx.notify();
    }

    fn finish_resize(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.resize_drag.take() else {
            return;
        };
        if !drag.changed {
            return;
        }

        self.preferred_size = sanitize_preferred_size(self.preferred_size);
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_document_browser_size(self.preferred_size)
        {
            eprintln!("failed to persist document browser size: {error:#}");
        }
    }

    fn render_resize_handle(
        axis: ResizeAxis,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (id, selector) = match axis {
            ResizeAxis::Width => (
                "document-browser-resize-width",
                "document-browser-resize-width",
            ),
            ResizeAxis::Height => (
                "document-browser-resize-height",
                "document-browser-resize-height",
            ),
            ResizeAxis::Both => (
                "document-browser-resize-corner",
                "document-browser-resize-corner",
            ),
        };

        div()
            .id(id)
            .debug_selector(move || selector.to_string())
            .absolute()
            .occlude()
            .map(|handle| match axis {
                ResizeAxis::Width => handle
                    .top_0()
                    .right_0()
                    .w(px(RESIZE_EDGE_SIZE))
                    .h_full()
                    .cursor_ew_resize()
                    .hover(move |style| style.bg(theme.primary.opacity(0.08))),
                ResizeAxis::Height => handle
                    .left_0()
                    .bottom_0()
                    .w_full()
                    .h(px(RESIZE_EDGE_SIZE))
                    .cursor_ns_resize()
                    .hover(move |style| style.bg(theme.primary.opacity(0.08))),
                ResizeAxis::Both => handle
                    .right_0()
                    .bottom_0()
                    .size(px(RESIZE_CORNER_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_nwse_resize()
                    .child(
                        Icon::new(ComponentIconName::ResizeCorner)
                            .size_3()
                            .text_color(theme.muted_foreground.opacity(0.58)),
                    ),
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.begin_resize(axis, event, window, cx);
                }),
            )
            .on_drag(DraggedDocumentBrowser(axis), |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| drag.clone())
            })
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<DraggedDocumentBrowser>, window, cx| {
                    if event.drag(cx).0 == axis {
                        this.resize_from_drag(event, window, cx);
                    }
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_resize(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_resize(cx)),
            )
            .into_any_element()
    }

    fn breadcrumbs(&self) -> Vec<(SharedString, PathBuf)> {
        if let Some(home) = dirs::home_dir()
            && let Ok(relative_path) = self.current_dir.strip_prefix(&home)
        {
            let mut path = home.clone();
            let mut breadcrumbs = vec![(crate::fl!("document-browser-home").into(), home)];
            for component in relative_path.components() {
                path.push(component.as_os_str());
                breadcrumbs.push((
                    component.as_os_str().to_string_lossy().into_owned().into(),
                    path.clone(),
                ));
            }
            return breadcrumbs;
        }

        let mut ancestors = self.current_dir.ancestors().collect::<Vec<_>>();
        ancestors.reverse();
        ancestors
            .into_iter()
            .map(|path| {
                let label = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| path.display().to_string());
                (label.into(), path.to_path_buf())
            })
            .collect()
    }

    fn key_cap(label: impl Into<SharedString>, theme: &ThemeColor) -> gpui::Div {
        div()
            .h(px(22.0))
            .px(px(6.0))
            .rounded(px(5.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.foreground.opacity(0.06))
            .text_size(ui_rems(10.5))
            .text_color(theme.muted_foreground.opacity(0.82))
            .child(label.into())
    }

    fn footer_hint(
        key: impl Into<SharedString>,
        label: impl Into<SharedString>,
        theme: &ThemeColor,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(px(5.0))
            .child(Self::key_cap(key, theme))
            .child(
                div()
                    .text_size(ui_rems(10.5))
                    .text_color(theme.muted_foreground.opacity(0.7))
                    .child(label.into()),
            )
    }

    fn footer_hint_pair(
        first: impl Into<SharedString>,
        second: impl Into<SharedString>,
        label: impl Into<SharedString>,
        theme: &ThemeColor,
    ) -> gpui::Div {
        let cap = div()
            .h(px(22.0))
            .px(px(5.0))
            .rounded(px(5.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(5.0))
            .bg(theme.foreground.opacity(0.06))
            .text_size(ui_rems(10.5))
            .text_color(theme.muted_foreground.opacity(0.82))
            .child(first.into())
            .child(
                div()
                    .w(px(1.0))
                    .h(px(11.0))
                    .bg(theme.foreground.opacity(0.1)),
            )
            .child(second.into());
        div().flex().items_center().gap(px(5.0)).child(cap).child(
            div()
                .text_size(ui_rems(10.5))
                .text_color(theme.muted_foreground.opacity(0.7))
                .child(label.into()),
        )
    }

    fn render_entry(
        &self,
        display_index: usize,
        entry: BrowserEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let selected = self.selected == display_index;
        let path = entry.path.clone();
        let kind = entry.kind;
        div()
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(8.0))
            .py(px(2.0))
            .child(
                div()
                    .id(("document-browser-entry", display_index))
                    .debug_selector(move || format!("document-browser-entry-{display_index}"))
                    .size_full()
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .cursor_pointer()
                    .border_1()
                    .border_color(if selected {
                        theme.list_active_border
                    } else {
                        transparent_black()
                    })
                    .bg(if selected {
                        theme.list_active
                    } else {
                        transparent_black()
                    })
                    .hover(move |style| style.bg(theme.list_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            if kind == EntryKind::Directory {
                                this.navigate_to(path.clone(), window, cx);
                            } else if event.click_count >= 2 {
                                cx.emit(DocumentBrowserEvent::Open(path.clone()));
                            } else {
                                this.selected = display_index;
                                cx.notify();
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        Icon::new(entry.kind.icon()).size(ui_rems(15.0)).text_color(
                            theme
                                .muted_foreground
                                .opacity(if entry.kind == EntryKind::Directory {
                                    0.86
                                } else {
                                    0.72
                                }),
                        ),
                    )
                    .child(
                        div()
                            .debug_selector(move || {
                                format!("document-browser-entry-label-{display_index}")
                            })
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.foreground)
                            .child(entry.name),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui_rems(10.5))
                            .text_color(theme.muted_foreground.opacity(0.78))
                            .child(entry.kind.label()),
                    ),
            )
            .into_any_element()
    }

    fn render_places(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .w(px(PLACES_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(8.0))
            .py(px(8.0))
            .border_r(px(1.0))
            .border_color(theme.border.opacity(0.72))
            .bg(theme.muted.opacity(0.16))
            .child(
                div()
                    .px(px(8.0))
                    .pt(px(2.0))
                    .pb(px(4.0))
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground.opacity(0.74))
                    .child(crate::fl!("document-browser-places")),
            )
            .children(self.places.iter().enumerate().map(|(index, place)| {
                let path = place.path.clone();
                let selected = self.current_dir == place.path;
                div()
                    .id(("document-browser-place", index))
                    .h(px(28.0))
                    .w_full()
                    .px(px(8.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .text_size(ui_rems(12.5))
                    .border_1()
                    .border_color(if selected {
                        theme.list_active_border
                    } else {
                        transparent_black()
                    })
                    .bg(if selected {
                        theme.list_active
                    } else {
                        transparent_black()
                    })
                    .text_color(if selected {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .when(!selected, |row| {
                        row.hover(|style| style.bg(theme.list_hover))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate_to(path.clone(), window, cx);
                        cx.stop_propagation();
                    }))
                    .child(
                        Icon::new(IconName::FolderOpen)
                            .size(ui_rems(14.0))
                            .text_color(theme.muted_foreground.opacity(0.82)),
                    )
                    .child(div().min_w_0().truncate().child(place.label.clone()))
            }))
            .into_any_element()
    }

    fn render_loading(_: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .px(px(16.0))
            .py(px(8.0))
            .children((0..7).map(|index| {
                Skeleton::new()
                    .h(px(24.0))
                    .w(relative(if index % 3 == 0 { 0.68 } else { 0.9 }))
                    .rounded(px(8.0))
            }))
            .into_any_element()
    }

    fn render_listing(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        match &self.load_state {
            LoadState::Loading => Self::render_loading(cx),
            LoadState::Error(error) => {
                let message = error.clone();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(10.0))
                    .px_6()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(message)
                    .child(
                        Button::new("document-browser-retry")
                            .small()
                            .outline()
                            .icon(IconName::RotateCcw)
                            .label(crate::fl!("document-browser-retry"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.load_directory(this.current_dir.clone(), cx);
                                cx.stop_propagation();
                            })),
                    )
                    .into_any_element()
            }
            LoadState::Ready if self.filtered.is_empty() => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(if self.input.read(cx).value().is_empty() {
                    crate::fl!("document-browser-empty-folder")
                } else {
                    crate::fl!("document-browser-no-results")
                })
                .into_any_element(),
            LoadState::Ready => {
                let count = self.filtered.len();
                let scroll = self.scroll.clone();
                div()
                    .size_full()
                    .overflow_hidden()
                    .child(
                        uniform_list(
                            "document-browser-list",
                            count,
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                range
                                    .filter_map(|display_index| {
                                        let entry_index = *this.filtered.get(display_index)?;
                                        let entry = this.entries.get(entry_index)?.clone();
                                        Some(this.render_entry(display_index, entry, cx))
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar_for_with_id(
                        &self.scroll,
                        "document-browser-scrollbar",
                        window,
                        cx,
                    )
                    .into_any_element()
            }
        }
    }
}

impl EventEmitter<DocumentBrowserEvent> for DocumentBrowser {}

impl Focusable for DocumentBrowser {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for DocumentBrowser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let actual_size = self.actual_size(window);
        let breadcrumbs = self.breadcrumbs();
        let last_crumb = breadcrumbs.len().saturating_sub(1);
        let open_shortcut: SharedString = if cfg!(target_os = "macos") {
            "⌘ O".into()
        } else {
            "Ctrl O".into()
        };
        let can_open = matches!(self.load_state, LoadState::Ready) && !self.filtered.is_empty();
        let band = theme.muted.opacity(0.28);
        let hairline = theme.border.opacity(0.72);

        div()
            .id("document-browser")
            .debug_selector(|| "document-browser".to_string())
            .key_context("DocumentBrowser")
            .track_focus(&self.modal_focus_handle)
            .relative()
            .w(px(actual_size.width))
            .h(px(actual_size.height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .popover_style(cx)
            .rounded(px(14.0))
            .border_1()
            .border_color(hairline)
            .bg(theme.popover)
            .shadow_xl()
            .text_size(ui_rems(12.5))
            .text_color(theme.popover_foreground)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.modal_focus_handle.contains_focused(window, cx) {
                        this.modal_focus_handle.focus(window, cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .h(px(46.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .pl(px(12.0))
                    .pr(px(10.0))
                    .border_b(px(1.0))
                    .border_color(hairline)
                    .bg(band)
                    .child(Self::key_cap(open_shortcut, &theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(ui_rems(14.0))
                            .child(Input::new(&self.input).appearance(false)),
                    )
                    .child(
                        div()
                            .id("document-browser-open")
                            .h(px(22.0))
                            .px(px(9.0))
                            .rounded(px(5.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .bg(theme.primary)
                            .text_size(ui_rems(11.5))
                            .text_color(theme.primary_foreground)
                            .when(!can_open, |chip| chip.opacity(0.55))
                            .hover(move |style| style.bg(theme.primary_hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_selected(window, cx);
                                cx.stop_propagation();
                            }))
                            .child("Enter"),
                    )
                    .child(
                        Self::key_cap("esc", &theme)
                            .id("document-browser-close")
                            .cursor_pointer()
                            .hover(move |style| style.bg(theme.foreground.opacity(0.1)))
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(DocumentBrowserEvent::Dismissed);
                                cx.stop_propagation();
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_places(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .bg(theme.background)
                            .child(
                                div()
                                    .h(px(38.0))
                                    .flex_none()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .px(px(13.0))
                                    .overflow_hidden()
                                    .text_size(ui_rems(11.0))
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .children(breadcrumbs.into_iter().enumerate().map(
                                        |(index, (label, path))| {
                                            let is_current = index == last_crumb;
                                            let crumb = div()
                                                .id(("document-browser-crumb", index))
                                                .px(px(3.0))
                                                .rounded(px(4.0))
                                                .text_color(if is_current {
                                                    theme.foreground.opacity(0.9)
                                                } else {
                                                    theme.muted_foreground.opacity(0.72)
                                                })
                                                .child(label);
                                            let crumb: AnyElement = if is_current {
                                                crumb.into_any_element()
                                            } else {
                                                crumb
                                                    .cursor_pointer()
                                                    .hover(move |style| {
                                                        style
                                                            .text_color(theme.foreground)
                                                            .bg(theme.list_hover)
                                                    })
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.navigate_to(
                                                                path.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                            cx.stop_propagation();
                                                        },
                                                    ))
                                                    .into_any_element()
                                            };
                                            div()
                                                .flex()
                                                .items_center()
                                                .when(index > 0, |item| {
                                                    item.child(
                                                        div()
                                                            .text_color(
                                                                theme
                                                                    .muted_foreground
                                                                    .opacity(0.55),
                                                            )
                                                            .child("/"),
                                                    )
                                                })
                                                .child(crumb)
                                        },
                                    )),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
                                    .child(self.render_listing(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(39.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(12.0))
                    .border_t(px(1.0))
                    .border_color(hairline)
                    .bg(band)
                    .child(Self::footer_hint_pair(
                        "↑",
                        "↓",
                        crate::fl!("document-browser-navigate"),
                        &theme,
                    ))
                    .child(Self::footer_hint(
                        "←",
                        crate::fl!("document-browser-up"),
                        &theme,
                    ))
                    .child(Self::footer_hint(
                        "→",
                        crate::fl!("document-browser-open"),
                        &theme,
                    )),
            )
            .children(
                [ResizeAxis::Width, ResizeAxis::Height, ResizeAxis::Both]
                    .map(|axis| Self::render_resize_handle(axis, theme, cx)),
            )
            .focus_trap("document-browser-focus-trap", &self.modal_focus_handle)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};
    use gpui_component::Root;

    use super::*;
    use crate::settings::SettingsStore;

    fn entry(name: &str, kind: EntryKind) -> BrowserEntry {
        BrowserEntry {
            path: PathBuf::from(name),
            name: name.into(),
            kind,
        }
    }

    #[test]
    fn folders_sort_before_documents_case_insensitively() {
        let mut entries = vec![
            entry("z.pdf", EntryKind::Pdf),
            entry("beta", EntryKind::Directory),
            entry("Alpha.epub", EntryKind::Epub),
            entry("alpha", EntryKind::Directory),
        ];
        DocumentBrowser::sort_entries(&mut entries);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_ref())
                .collect::<Vec<_>>(),
            ["alpha", "beta", "Alpha.epub", "z.pdf"]
        );
    }

    #[test]
    fn filter_ranks_prefix_matches_before_substring_matches() {
        let entries = vec![
            entry("my-paper.pdf", EntryKind::Pdf),
            entry("paperwork", EntryKind::Directory),
            entry("wallpaper.epub", EntryKind::Epub),
        ];

        let filtered = DocumentBrowser::filter_indices("paper", &entries);

        assert_eq!(filtered, vec![1, 0, 2]);
    }

    #[test]
    fn preferred_size_is_sanitized_to_comet_geometry() {
        assert_eq!(
            sanitize_preferred_size(DocumentBrowserSize {
                width: 120.0,
                height: 200.0,
            }),
            default_browser_size()
        );
        assert_eq!(
            sanitize_preferred_size(DocumentBrowserSize {
                width: f32::NAN,
                height: f32::INFINITY,
            }),
            default_browser_size()
        );
    }

    #[test]
    fn fitted_size_restores_the_unchanged_preference_when_the_window_grows() {
        let preferred = DocumentBrowserSize {
            width: 920.0,
            height: 640.0,
        };

        assert_eq!(
            fit_browser_size(preferred, 700.0, 440.0),
            DocumentBrowserSize {
                width: 668.0,
                height: 408.0,
            }
        );
        assert_eq!(fit_browser_size(preferred, 1200.0, 800.0), preferred);
    }

    #[gpui::test]
    fn moving_selection_does_not_shift_document_entry_content(cx: &mut TestAppContext) {
        let root_dir = std::env::temp_dir().join(format!(
            "glyphst-document-browser-selection-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root_dir).unwrap();
        fs::write(root_dir.join("alpha.pdf"), b"").unwrap();
        fs::write(root_dir.join("beta.pdf"), b"").unwrap();

        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let browser = cx.new(|cx| DocumentBrowser::new(root_dir.clone(), window, cx));
            Root::new(browser, window, cx)
        });
        let browser = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<DocumentBrowser>().unwrap()
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        browser.read_with(cx, |browser, _| {
            assert_eq!(browser.filtered.len(), 2);
            assert_eq!(browser.selected, 0);
        });

        let first_row = cx.debug_bounds("document-browser-entry-0").unwrap();
        let second_row = cx.debug_bounds("document-browser-entry-1").unwrap();
        let first_label = cx.debug_bounds("document-browser-entry-label-0").unwrap();
        let second_label = cx.debug_bounds("document-browser-entry-label-1").unwrap();
        let first_offset = first_label.left() - first_row.left();
        let second_offset = second_label.left() - second_row.left();
        assert_eq!(first_offset, second_offset);

        cx.simulate_keystrokes("down");
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        browser.read_with(cx, |browser, _| assert_eq!(browser.selected, 1));

        let first_row_after_down = cx.debug_bounds("document-browser-entry-0").unwrap();
        let second_row_after_down = cx.debug_bounds("document-browser-entry-1").unwrap();
        let first_label_after_down = cx.debug_bounds("document-browser-entry-label-0").unwrap();
        let second_label_after_down = cx.debug_bounds("document-browser-entry-label-1").unwrap();
        assert_eq!(first_row_after_down, first_row);
        assert_eq!(second_row_after_down, second_row);
        assert_eq!(
            first_label_after_down.left() - first_row_after_down.left(),
            first_offset
        );
        assert_eq!(
            second_label_after_down.left() - second_row_after_down.left(),
            second_offset
        );

        cx.simulate_keystrokes("up");
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        browser.read_with(cx, |browser, _| assert_eq!(browser.selected, 0));
        assert_eq!(
            cx.debug_bounds("document-browser-entry-label-0")
                .unwrap()
                .left()
                - cx.debug_bounds("document-browser-entry-0").unwrap().left(),
            first_offset
        );
        assert_eq!(
            cx.debug_bounds("document-browser-entry-label-1")
                .unwrap()
                .left()
                - cx.debug_bounds("document-browser-entry-1").unwrap().left(),
            second_offset
        );

        fs::remove_dir_all(root_dir).unwrap();
    }

    #[test]
    fn directory_reader_filters_unsupported_and_hidden_files() {
        let root = std::env::temp_dir().join(format!(
            "glyphst-document-browser-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("folder")).unwrap();
        fs::write(root.join("paper.PDF"), b"").unwrap();
        fs::write(root.join("book.epub"), b"").unwrap();
        fs::write(root.join("notes.txt"), b"").unwrap();
        fs::write(root.join(".hidden.pdf"), b"").unwrap();

        let entries = DocumentBrowser::read_directory(&root, false).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().all(|entry| !entry.name.starts_with('.')));

        fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn directory_reader_treats_symlinked_directories_as_folders() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "glyphst-document-browser-symlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, root.join("linked-folder")).unwrap();

        let entries = DocumentBrowser::read_directory(&root, false).unwrap();
        assert!(entries.iter().any(|entry| {
            entry.name.as_ref() == "linked-folder" && entry.kind == EntryKind::Directory
        }));

        fs::remove_dir_all(root).ok();
    }
}
